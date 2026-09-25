//! Streaming: chunk-by-chunk repair must equal whole-document repair.

use jsonfix::{Allow, Options, StreamRepairer};

const DOCUMENT: &str = "{\"title\": \"Report\", \"items\": [1, 2, 3], \"ok\": True,}";
const REPAIRED: &str = "{\"title\": \"Report\", \"items\": [1, 2, 3], \"ok\": true}";

#[test]
fn every_split_matches_the_whole() {
    for split in 0..=DOCUMENT.len() {
        if !DOCUMENT.is_char_boundary(split) {
            continue;
        }
        let mut stream = StreamRepairer::new();
        let mut rendered = None;
        for chunk in [&DOCUMENT[..split][..], &DOCUMENT[split..][..]] {
            if chunk.is_empty() {
                continue;
            }
            rendered = Some(String::from(stream.push(chunk).expect("chunk")));
        }
        assert_eq!(
            rendered.as_deref().unwrap_or(stream.output()),
            REPAIRED,
            "split at {split}"
        );
    }
}

#[test]
fn character_by_character_matches_the_whole() {
    let mut stream = StreamRepairer::new();
    let mut rendered = String::new();
    for chunk in DOCUMENT.chars() {
        let text = stream.push(&chunk.to_string()).expect("chunk");
        rendered = String::from(text);
    }
    assert_eq!(rendered, REPAIRED);
    assert_eq!(stream.input(), DOCUMENT);
    assert_eq!(stream.output(), REPAIRED);
    assert_eq!(stream.len(), DOCUMENT.len());
}

#[test]
fn deltas_rebuild_the_output() {
    let mut stream = StreamRepairer::new();
    let mut buffer = String::new();
    for chunk in ["{\"a\": ", "1, \"b\": \"x", "y\", \"c\": [1,", " 2]}"] {
        let delta = stream.push_delta(chunk).expect("delta");
        buffer.truncate(delta.keep);
        buffer.push_str(delta.text);
        assert_eq!(buffer, stream.output());
    }
    assert_eq!(buffer, "{\"a\": 1, \"b\": \"xy\", \"c\": [1, 2]}");
}

#[test]
fn a_truncated_stream_is_readable_at_every_step() {
    let mut stream = StreamRepairer::with_options(Options::partial(Allow::ALL));
    let mut last = String::new();
    for chunk in ["{\"steps\": [", "{\"done\": tru", "e}, {\"done\": fal"] {
        last = String::from(stream.push(chunk).expect("partial render"));
        // Whatever the model has produced so far, the render must be valid JSON.
        jsonfix::validate(&last).unwrap_or_else(|error| panic!("{last:?}: {error}"));
    }
    assert_eq!(last, "{\"steps\": [{\"done\": true}, {\"done\": false}]}");
}

#[test]
fn reset_starts_a_new_document() {
    let mut stream = StreamRepairer::new();
    assert!(stream.is_empty());
    stream.push("{\"a\": 1}").unwrap();
    assert!(!stream.is_empty());
    stream.reset();
    assert!(stream.is_empty());
    assert_eq!(stream.push("[2]").unwrap(), "[2]");
}

#[test]
fn a_value_is_available_without_rendering() {
    let mut stream = StreamRepairer::new();
    stream.push("{\"a\": [1, 2").unwrap();
    let value = stream.value().expect("partial value");
    assert_eq!(value.pointer("/a/1").and_then(|v| v.as_i64()), Some(2));
}

#[test]
fn colon_at_eof_does_not_checkpoint_a_synthetic_null() {
    // `{"a":` renders `{"a": null}` by truncation repair — but that null is
    // EOF-invented: the next chunk may supply a real value instead.
    let mut stream = jsonfix::StreamRepairer::new();
    let first = stream.push("{\"a\":").unwrap().to_string();
    assert_eq!(first, "{\"a\": null}");
    let second = stream.push("1}").unwrap().to_string();
    assert_eq!(second, "{\"a\": 1}");
}

#[test]
fn double_escape_detection_is_exact_across_chunk_boundaries() {
    // A double-escaped document must unescape identically no matter where the
    // input is split across chunks — including splits inside a backslash run
    // (`\\` + `\"`) and before the quote that decides the verdict. Any
    // streaming cache of this verdict must match the whole-input test
    // (`crate::looks_double_escaped`) at every prefix.
    let hardcoded: &[(&str, &str)] = &[
        (r#"\"a\": 1"#, r#"{"a": 1}"#),
        (r#""a\" b""#, r#""a\" b""#),
        (r#"{\"a\": \"b\"}"#, r#"{"a": "b"}"#),
        (r#"{\"k\": \"b\\\"}"#, r#"{"k": "b\""}"#),
    ];
    for (input, want) in hardcoded.iter().copied() {
        let want = want.to_string();
        for split in 0..=input.len() {
            if !input.is_char_boundary(split) {
                continue;
            }
            let mut stream = StreamRepairer::new();
            let mut acc = String::new();
            let mut rolled_back = false;
            for chunk in [&input[..split], &input[split..]] {
                if chunk.is_empty() {
                    continue;
                }
                acc.push_str(chunk);
                match stream.push(chunk) {
                    Ok(got) => assert_eq!(
                        got,
                        jsonfix::repair(&acc).expect("prefix repairs"),
                        "prefix {acc:?}"
                    ),
                    Err(_) => {
                        acc.truncate(acc.len() - chunk.len());
                        rolled_back = true;
                    }
                }
            }
            if !rolled_back {
                assert_eq!(stream.output(), want, "split at {split} of {input:?}");
            }
        }
    }
}

#[test]
fn growing_object_matches_repair_at_every_prefix() {
    let mut doc = String::from("{\"title\": \"x\",");
    for i in 0..40 {
        doc.push_str(&format!("\"k{i}\": {i}, \"s{i}\": \"value {i}\", "));
    }
    doc.push_str("\"end\": true}");
    let mut stream = jsonfix::StreamRepairer::new();
    let mut acc = String::new();
    let mut start = 0;
    // odd-sized chunks so checkpoints land in many places
    let step = 23;
    while start < doc.len() {
        let mut end = (start + step).min(doc.len());
        while end > start && !doc.is_char_boundary(end) {
            end -= 1;
        }
        acc.push_str(&doc[start..end]);
        let out = stream.push(&doc[start..end]).unwrap().to_string();
        let want = jsonfix::repair(&acc).unwrap();
        assert_eq!(out, want, "prefix mismatch at byte {end}");
        start = end;
    }
    assert_eq!(stream.output(), jsonfix::repair(&doc).unwrap());
}

#[test]
fn ndjson_line_by_line_matches_repair() {
    let mut doc = String::new();
    for i in 0..60 {
        doc.push_str(&format!("{{\"n\": {i}, \"tag\": \"line-{i}\"}}\n"));
    }
    let mut stream = jsonfix::StreamRepairer::new();
    let mut acc = String::new();
    for line in doc.split_inclusive('\n') {
        acc.push_str(line);
        let out = stream.push(line).unwrap().to_string();
        assert_eq!(out, jsonfix::repair(&acc).unwrap(), "at {acc:?}");
    }
}

#[test]
fn fence_markers_disable_resume_for_stable_parity() {
    // A later ``` can re-interpret bytes already checkpointed
    // (`v,` alone → ["v", ""], `v,```j` → "v"). After any backtick the
    // stream must full-reparse so it matches `repair(prefix)`.
    let mut stream = StreamRepairer::new();
    let mut acc = String::new();
    for chunk in ["v,`", "``j", "son"] {
        acc.push_str(chunk);
        let got = String::from(stream.push(chunk).unwrap());
        let want = jsonfix::repair(&acc).unwrap();
        assert_eq!(got, want, "prefix {acc:?}");
    }
}

#[test]
fn ndjson_bracket_insert_keeps_checkpoints_consistent() {
    // Regression: after retrofitting `[`, a TopContinue checkpoint from
    // before the insert re-inserted a second `[` on resume
    // (`1, 2;…` → `[[1, …]]` instead of `[1, …]`).
    let head = "1, 2;\0\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}";
    let tail = "\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}\u{e6}";
    let mut stream = StreamRepairer::new();
    let first = String::from(stream.push(head).unwrap());
    let full = format!("{head}{tail}");
    let second = String::from(stream.push(tail).unwrap());
    assert_eq!(first, jsonfix::repair(head).unwrap());
    assert_eq!(second, jsonfix::repair(&full).unwrap());
    assert!(!second.starts_with("[["), "double bracket: {second:?}");
}

#[test]
fn failed_push_invalidates_checkpoints() {
    // A failed parse may write `cp` into the discarded tail; the next push
    // must not resume from it (`is_char_boundary` panic on multi-byte input).
    let mut stream = StreamRepairer::new();
    // Valid prefix with multi-byte chars.
    let good = "invalid \u{FFFD}\u{FFFD} utf8";
    // This alone is TrailingValue (prose + values without extract()).
    let _ = stream.push(good);
    // Append something that fails; rollback must clear resume state.
    let _ = stream.push(" {\"a\": 1}");
    // Further pushes must not panic and must match repair-or-error.
    let mut acc = String::from(good);
    let _ = stream.push(" more");
    acc.push_str(" more");
    if let Ok(out) = stream.push(" {}") {
        assert_eq!(out, jsonfix::repair(&format!("{acc} {{}}")).unwrap());
    }
}

#[test]
fn truncated_string_disables_checkpoints_for_the_rest_of_the_document() {
    // Prefix ending *on* a delimiter lets the two-pass rule close an
    // unclosed string at a comma (so the comma looks like a member
    // separator). Appending more input means the string no longer stops
    // there — commas become string content — so any checkpoint taken after
    // a truncated string is invalid.
    let mut stream = StreamRepairer::new();
    let mut acc = String::new();
    // Build the minimal shape: unclosed string, then more text after a comma.
    for chunk in ["{\"k\": \"a", ", \"b\": 1", "}"] {
        acc.push_str(chunk);
        let got = stream.push(chunk).map(String::from);
        let want = jsonfix::repair(&acc);
        if got.is_err() && want.is_err() {
            let keep = acc.len() - chunk.len();
            acc.truncate(keep);
        } else {
            assert_eq!(got, want, "prefix {acc:?}");
        }
    }
}

#[test]
fn fuzz_crash_two_pass_string_comma_is_not_stable() {
    // Exact fuzzer input: crash-8f3c4a8fbdd1d96392d12fe24c4992249442d652
    let data: &[u8] = &[
        34, 123, 49, 97, 255, 125, 34, 255, 52, 114, 117, 101, 44, 32, 70, 97, 108, 115, 101, 44,
        32, 78, 111, 110, 254, 255, 255, 255, 70, 97, 108, 115, 101, 91, 125, 32, 36, 58, 125,
    ];
    let (&steer, rest) = data.split_first().expect("nonempty");
    let step = 1 + usize::from(steer % 32);
    let text = String::from_utf8_lossy(rest);
    let text = text.as_ref();
    let mut stream = StreamRepairer::new();
    let mut acc = String::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + step).min(text.len());
        if end < text.len() {
            while end < text.len() && !text.is_char_boundary(end) {
                end += 1;
            }
        }
        let chunk = &text[start..end];
        acc.push_str(chunk);
        let got = stream.push(chunk).map(String::from);
        let want = jsonfix::repair(&acc);
        if got.is_err() && want.is_err() {
            acc.truncate(acc.len() - chunk.len());
        } else {
            assert_eq!(got, want, "prefix {acc:?}");
        }
        start = end;
    }
}
