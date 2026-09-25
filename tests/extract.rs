//! Extraction: pulling a value out of prose, fences, and log lines.

use jsonfix::{extract, extract_all, extract_partial, repair_extract};

#[test]
fn fenced_blocks_win() {
    let reply = "Sure!\n```json\n{\"a\": [1, 2]}\n```\nDone.";
    assert_eq!(extract(reply), Some("{\"a\": [1, 2]}"));
    assert_eq!(repair_extract(reply).unwrap(), "{\"a\": [1, 2]}");
}

#[test]
fn json_fence_beats_other_fences() {
    let text = "step one\n```py\nprint(1)\n```\nthen\n```json\n[1]\n```\n";
    assert_eq!(extract(text), Some("[1]"));
}

#[test]
fn loose_values_are_found() {
    assert_eq!(extract("Here: {\"a\": 1} and more"), Some("{\"a\": 1}"));
    assert_eq!(extract("[1, 2] then text"), Some("[1, 2]"));
    assert_eq!(extract("answer is \"quoted\""), Some("\"quoted\""));
    assert_eq!(extract("42 apples"), Some("42"));
    assert_eq!(extract("   {\"a\": 1}   "), Some("{\"a\": 1}"));
}

#[test]
fn brackets_inside_strings_and_comments_do_not_confuse_it() {
    assert_eq!(extract("{\"a\": \"}\"}"), Some("{\"a\": \"}\"}"));
    assert_eq!(extract("{\"a\": 1 /* } */ }"), Some("{\"a\": 1 /* } */ }"));
    assert_eq!(
        extract("{\"a\": [1, [2, [3]]]}"),
        Some("{\"a\": [1, [2, [3]]]}")
    );
    assert_eq!(
        extract("{\"a\": \"it's {ok}\"}"),
        Some("{\"a\": \"it's {ok}\"}")
    );
}

#[test]
fn nothing_to_extract() {
    assert_eq!(extract("no json here"), None);
    assert_eq!(extract(""), None);
    assert_eq!(extract("```\n```"), None);
    assert_eq!(extract_partial("still typing..."), "");
    assert!(extract_all("nothing").is_empty());
}

#[test]
fn multiple_values() {
    let log = "{\"n\": 1}\n{\"n\": 2}";
    assert_eq!(extract_all(log), vec!["{\"n\": 1}", "{\"n\": 2}"]);
    assert_eq!(extract_all("[1] [2] [3]"), vec!["[1]", "[2]", "[3]"]);
}

#[test]
fn partial_extraction_keeps_the_tail() {
    assert_eq!(extract_partial("partial: {\"a\": [1, 2"), "{\"a\": [1, 2");
    assert_eq!(extract_partial("```json\n{\"a\": 1"), "{\"a\": 1");
    assert_eq!(extract_partial("\"just star"), "\"just star");
}

#[test]
fn extracted_values_are_repairable() {
    let log_line = "2024-05-01T10:00:00Z INFO payload={user: 'ada', ok: True,}";
    let span = extract(log_line).expect("has a value");
    assert_eq!(span, "{user: 'ada', ok: True,}");
    assert_eq!(
        jsonfix::repair(span).unwrap(),
        "{\"user\": \"ada\", \"ok\": true}"
    );
}
