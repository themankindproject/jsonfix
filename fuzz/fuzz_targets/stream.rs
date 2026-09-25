//! Chunked streaming must equal whole-document repair at every prefix, and
//! `push_delta` must rebuild exactly what `push` renders.

#![no_main]

use jsonfix::{StreamRepairer, repair};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // First byte steers chunk size (1..=32) so libFuzzer explores boundaries;
    // the rest is the document — including non-UTF-8, lossy-repaired.
    let (&steer, rest) = match data.split_first() {
        Some(pair) => pair,
        None => {
            // Empty input: push("") must be a no-op render, not a panic.
            let mut stream = StreamRepairer::new();
            let _ = stream.push("");
            assert_eq!(stream.output(), "");
            return;
        }
    };
    let step = 1 + usize::from(steer % 32);
    let input = String::from_utf8_lossy(rest);
    let text = input.as_ref();

    // --- push: parity with repair at every prefix, error kind included ---
    let mut stream = StreamRepairer::new();
    let mut acc = String::new();
    let mut start = 0usize;
    while start < text.len() {
        let end = chunk_end(text, start, step);
        let chunk = &text[start..end];
        acc.push_str(chunk);

        let whole = repair(&acc);
        match (stream.push(chunk), whole) {
            (Ok(streamed), Ok(want)) => assert_eq!(
                streamed,
                want.as_str(),
                "stream diverged at byte {end} of {text:?}"
            ),
            (Err(got), Err(want)) => {
                assert_eq!(
                    got.kind(),
                    want.kind(),
                    "stream and repair disagree on error for prefix {acc:?}"
                );
                // `push` rolled its input back — mirror that in `acc` so the
                // next prefix comparison uses the same document the stream holds.
                acc.truncate(acc.len() - chunk.len());
            }
            (Ok(got), Err(want)) => {
                panic!("stream repaired {acc:?} to {got:?} but repair failed: {want}")
            }
            (Err(got), Ok(want)) => {
                panic!("stream failed on {acc:?}: {got}; repair returned {want}")
            }
        }
        start = end;
    }
    // Final `output()` must match repairing whatever the stream accepted.
    let final_want = repair(&acc).map(String::from);
    match (String::from(stream.output()), final_want) {
        (got, Ok(want)) => assert_eq!(got, want, "final output diverged for {text:?}"),
        (got, Err(e)) if !got.is_empty() => {
            panic!("stream kept {got:?} but repair of its input failed: {e}")
        }
        _ => {}
    }

    // --- push_delta: applying keep/text must reproduce push's output ---
    let mut delta_stream = StreamRepairer::new();
    let mut buffer = String::new();
    let mut pos = 0usize;
    while pos < text.len() {
        let end = chunk_end(text, pos, step);
        let chunk = &text[pos..end];
        match delta_stream.push_delta(chunk) {
            Ok(delta) => {
                // Copy out of the borrow so `output()` can re-borrow the stream.
                let keep = delta.keep;
                let text = delta.text.to_string();
                assert!(
                    keep <= buffer.len(),
                    "delta keep {keep} exceeds buffer len {}",
                    buffer.len()
                );
                buffer.truncate(keep);
                buffer.push_str(&text);
                assert_eq!(
                    buffer,
                    delta_stream.output(),
                    "applying delta (keep={keep}, text={text:?}) diverged at byte {end}"
                );
            }
            Err(_) => {
                // Chunk rolled back: our mirror must still match the stream.
                assert_eq!(
                    buffer,
                    delta_stream.output(),
                    "rollback at byte {end} left buffer and output() diverged"
                );
            }
        }
        pos = end;
    }

    // --- reset starts a fresh document ---
    delta_stream.reset();
    assert_eq!(delta_stream.output(), "");
    if !text.is_empty() {
        let after = delta_stream.push(text).map(String::from);
        let want = repair(text);
        match (after, want) {
            (Ok(got), Ok(want)) => assert_eq!(got, want.as_str(), "post-reset diverged"),
            (Err(got), Err(want)) => assert_eq!(got.kind(), want.kind()),
            (a, b) => panic!("post-reset reset parity broke: {a:?} vs {b:?}"),
        }
    }
});

/// End of the next chunk: `start + step`, snapped forward to a char boundary
/// (forward, so multi-byte characters always make progress).
fn chunk_end(text: &str, start: usize, step: usize) -> usize {
    let target = (start + step).min(text.len());
    if target == text.len() {
        return text.len();
    }
    let mut end = target;
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    // step >= 1 ⇒ target > start ⇒ end > start after snapping forward.
    debug_assert!(end > start);
    end
}
