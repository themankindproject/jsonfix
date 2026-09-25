//! Property tests: the invariants that must hold for *every* input.

use jsonfix::{Allow, Options, StreamRepairer, extract, parse, repair};
use proptest::prelude::*;
use proptest::test_runner::Config;

/// Generates valid JSON text.
///
/// Values never contain backticks or invalid escapes, so the text is exactly a
/// JSON document and nothing else.
fn json_text() -> impl Strategy<Value = String> {
    let string_leaf =
        "[A-Za-z0-9 _.:-]{0,12}".prop_map(|text| serde_json::to_string(&text).expect("quoted"));
    let leaf = prop_oneof![
        Just("null".to_string()),
        Just("true".to_string()),
        Just("false".to_string()),
        any::<i64>().prop_map(|value| value.to_string()),
        (any::<u32>(), 0u8..60).prop_map(|(value, shift)| format!("{}", u64::from(value) << shift)),
        string_leaf,
    ];
    leaf.prop_recursive(3, 24, 6, |inner| {
        let array = prop::collection::vec(inner.clone(), 0..5)
            .prop_map(|items| format!("[{}]", items.join(",")));
        let object = prop::collection::vec(("[A-Za-z_][A-Za-z0-9_]{0,6}", inner.clone()), 0..5)
            .prop_map(|members| {
                let body = members
                    .iter()
                    .map(|(key, value)| format!("{key:?}:{value}"))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{{{body}}}")
            });
        prop_oneof![array, object]
    })
}

fn parsed(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{text:?}: {error}"))
}

proptest! {
    #![proptest_config(Config { cases: 512, ..Config::default() })]

    /// Repairing valid JSON must not change what it means.
    #[test]
    fn repair_preserves_valid_json(text in json_text()) {
        let repaired = repair(&text).expect("valid JSON repairs");
        prop_assert_eq!(parsed(&repaired), parsed(&text));
        prop_assert_eq!(parse(&text).expect("parses").to_json_string(), repaired);
    }

    /// Repairing twice is the same as repairing once.
    #[test]
    fn repair_is_a_fixpoint(text in json_text()) {
        let once = repair(&text).expect("valid JSON repairs");
        let twice = repair(&once).expect("repaired output is valid JSON");
        prop_assert_eq!(once, twice);
    }

    /// A repaired document can always be read back by another parser.
    #[test]
    fn repair_output_is_always_valid_json(input in "\\PC{0,120}") {
        if let Ok(repaired) = repair(&input) {
            prop_assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok(),
                "repaired {:?} to {:?}", input, repaired);
        }
    }

    /// The same is true of the tree rendering, and it is the same text.
    #[test]
    fn parse_and_repair_agree(input in "\\PC{0,120}") {
        let value = match parse(&input) {
            Ok(value) => value,
            Err(_) => return Ok(()),
        };
        let rendered = value.to_json_string();
        prop_assert!(serde_json::from_str::<serde_json::Value>(&rendered).is_ok(),
            "rendered {:?} from {:?}", rendered, input);
    }

    /// Streaming in two pieces equals repairing the whole document.
    #[test]
    fn streaming_matches_whole_document(text in json_text(), split in 0usize..64) {
        let split = split.min(text.len());
        let split = (0..=split).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
        let (first, second) = text.split_at(split);
        let mut stream = StreamRepairer::new();
        let mut streamed = None;
        for chunk in [first, second] {
            if chunk.is_empty() {
                continue;
            }
            streamed = Some(String::from(stream.push(chunk).expect("chunk")));
        }
        let streamed = streamed.unwrap_or_else(|| String::from(stream.output()));
        prop_assert_eq!(streamed, repair(&text).expect("valid JSON repairs"));
    }

    /// Extraction finds the whole document when the text is only JSON.
    ///
    /// Bare `null`/`true`/`false` leaves are deliberately not extracted: a
    /// keyword is indistinguishable from prose.
    #[test]
    fn extraction_finds_bare_documents(text in json_text()) {
        prop_assume!(
            text.starts_with('{')
                || text.starts_with('[')
                || text.starts_with('"')
                || text.starts_with(|c: char| c.is_ascii_digit() || c == '-'),
            "keyword leaves are prose-shaped"
        );
        prop_assert_eq!(extract(&text), Some(text.as_str()));
    }

    /// A double-escaped document (quotes escaped, no outer quotes) still parses.
    #[test]
    fn double_escaped_documents_are_unescaped(text in json_text()) {
        let escaped = text.replace('"', "\\\"");
        prop_assert_eq!(repair(&escaped).expect("escaped repairs"), repair(&text).expect("repairs"));
    }

    /// A valid document can be rendered as a partial parse at any point.
    #[test]
    fn partial_render_of_a_prefix_is_valid(text in json_text(), cut in 0usize..64) {
        let cut = cut.min(text.len());
        let cut = (0..=cut).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
        let opts = Options::partial(Allow::ALL);
        if let Ok(value) = jsonfix::parse_partial(&text[..cut], opts) {
            let rendered = value.to_json_string();
            prop_assert!(serde_json::from_str::<serde_json::Value>(&rendered).is_ok(), "{}", rendered);
        }
    }
}
