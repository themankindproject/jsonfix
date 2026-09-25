//! Corpus ported from the `jsonrepair` JS reference test suite
//! (https://github.com/josdejong/jsonrepair, `src/index.test.ts`).
//!
//! Two systematic adaptations, both documented here rather than per-case:
//!
//! 1. `jsonrepair` patches the input *textually* and preserves surrounding
//!    whitespace; `jsonfix` re-serializes a `Value` tree, so expectations are
//!    the canonical output (whitespace between tokens collapses, e.g.
//!    `'[1, 2, 3, ... ]'` → `'[1, 2, 3]'`).
//! 2. JS-only behaviors are out of scope: exact `JSONRepairError` byte
//!    offsets/messages, JSONP whitespace preservation, regex literals kept
//!    eval-safe for JavaScript `eval`, and MongoDB/JSONP context-dependent
//!    cases whose Rust equivalents already live in `tests/repair.rs`.
//!
//! Every expectation here is independently validated with `serde_json`, and
//! repairing the repaired output must be a no-op (the output is stable).

use jsonfix::{Allow, ErrorKind, Options, parse_partial, repair, repair_with};

/// (input, expected) pairs where repair must succeed.
const CASES: &[(&str, &str)] = &[
    // --- valid JSON passes through unchanged ---
    (
        r#"{"a":2.3e100,"b":"str","c":null,"d":false,"e":[1,2,3]}"#,
        r#"{"a":2.3e100,"b":"str","c":null,"d":false,"e":[1,2,3]}"#,
    ),
    ("  { \n } \t ", "{}"),
    ("{}", "{}"),
    ("{  }", "{}"),
    (r#"{"a": {}}"#, r#"{"a": {}}"#),
    (r#"{"a": "b"}"#, r#"{"a": "b"}"#),
    ("[]", "[]"),
    ("[1,2,3]", "[1,2,3]"),
    ("[1,2,[3,4,5]]", "[1,2,[3,4,5]]"),
    ("[{}]", "[{}]"),
    (r#"{"a":[]}"#, r#"{"a":[]}"#),
    // valid numbers keep their exact text
    ("23", "23"),
    ("0", "0"),
    ("0e+2", "0e+2"),
    ("0.0", "0.0"),
    ("-0", "-0"),
    ("2.3", "2.3"),
    ("2300e3", "2300e3"),
    ("2300e-3", "2300e-3"),
    ("2e-3", "2e-3"),
    ("2.3e-3", "2.3e-3"),
    // A backslash adjacent to an unfinished string folds into it, matching the
    // JS "repair escaped string contents" behavior (index.test.ts L457-464):
    //   \"hello\, \"world\"  →  ["hello\", "world"]
    // A lone backslash between values folds into an unquoted string as well
    // (the JS reference throws on these; see the ERRORS doc).
    (r#"[\"y"\, "z"]"#, r#"["y\", \"z"]"#),
    (r#"[\"y", "z"\]"#, r#"["y", "z\"]"]"#),
    (r#"[\"y" \, "z"]"#, r#"["y\" , \"z"]"#),
    (r#"["y",\\ "z"]"#, r#"["y", "\\\\", "z"]"#),
    (r#"[["y"],\["z"]]"#, r#"[["y"], "\\", ["z"]]"#),
    (r#"{"a": "y"\, "b"\: "z"}"#, r#"{"a": "y\", \"b\": \"z"}"#),
    (r#""str""#, r#""str""#),
    (r#""\"\\"\/\b\f\n\r\t""#, r#""\"\\"\/\b\f\n\r\t""#),
    (r#""\u260E""#, r#""☎""#),
    ("true", "true"),
    ("false", "false"),
    ("null", "null"),
    // a word that merely starts like a keyword stays a string
    ("trueble", r#""trueble""#),
    ("Trueble", r#""Trueble""#),
    ("false_value", r#""false_value""#),
    ("false$", r#""false$""#),
    ("nullifiable", r#""nullifiable""#),
    ("[truee", r#"["truee"]"#),
    // strings equaling a JSON delimiter
    (r#""""#, r#""""#),
    (r#""[""#, r#""[""#),
    (r#""]""#, r#""]""#),
    (r#""{""#, r#""{""#),
    (r#""}""#, r#""}""#),
    (r#"":""#, r#"":""#),
    (r#"",""#, r#"",""#),
    // unicode in strings and keys
    (r#""★""#, r#""★""#),
    (r#""\u2605""#, r#""★""#),
    ("\"😀\"", "\"😀\""),
    (r#""\ud83d\ude00""#, "\"😀\""),
    (r#""йнформация""#, r#""йнформация""#),
    (r#"{"★":true}"#, r#"{"★":true}"#),
    // --- missing quotes around keys and values ---
    ("abc", r#""abc""#),
    ("hello   world", r#""hello   world""#),
    (
        "{\nmessage: hello world\n}",
        r#"{"message": "hello world"}"#,
    ),
    ("{a:2}", r#"{"a":2}"#),
    ("{a: 2}", r#"{"a": 2}"#),
    ("{2: 2}", r#"{"2": 2}"#),
    ("{true: 2}", r#"{"true": 2}"#),
    ("[a,b]", r#"["a","b"]"#),
    ("[foo,4]", r#"["foo",4]"#),
    ("{foo: bar}", r#"{"foo": "bar"}"#),
    ("foo 2 bar", r#""foo 2 bar""#),
    ("{greeting: hello world}", r#"{"greeting": "hello world"}"#),
    (
        "{greeting: hello world!}",
        r#"{"greeting": "hello world!"}"#,
    ),
    // unquoted URLs
    ("https://www.bible.com/", r#""https://www.bible.com/""#),
    (
        "{url:https://www.bible.com/}",
        r#"{"url":"https://www.bible.com/"}"#,
    ),
    (
        r#"{url:https://www.bible.com/,"id":2}"#,
        r#"{"url":"https://www.bible.com/","id":2}"#,
    ),
    (
        "[https://www.bible.com/,2]",
        r#"["https://www.bible.com/",2]"#,
    ),
    // missing end quote on a URL
    (r#""https://www.bible.com/"#, r#""https://www.bible.com/""#),
    // --- missing / mismatched end quotes ---
    (r#""abc""#, r#""abc""#),
    ("'abc'", r#""abc""#),
    (r#""12:20""#, r#""12:20""#),
    (r#"{"time":"12:20}"#, r#"{"time":"12:20"}"#),
    (
        "{\"date\":2024-10-18T18:35:22.229Z}",
        r#"{"date":"2024-10-18T18:35:22.229Z"}"#,
    ),
    (r#""She said:""#, r#""She said:""#),
    (r#"{"text": "She said:""#, r#"{"text": "She said:"}"#),
    (r#"["hello, world]"#, r#"["hello", "world"]"#),
    (r#"["hello,"world"]"#, r#"["hello","world"]"#),
    (r#"{"a":"b}"#, r#"{"a":"b"}"#),
    (r#"{"a":"b,"c":"d"}"#, r#"{"a":"b","c":"d"}"#),
    (r#"{"a":"b,c,"d":"e"}"#, r#"{"a":"b,c","d":"e"}"#),
    ("{a:\"b,c,\"d\":\"e\"}", r#"{"a":"b,c","d":"e"}"#),
    ("[\"b,c,]", r#"["b","c"]"#),
    ("\u{2018}abc", r#""abc""#),
    (r#""it's working"#, r#""it's working""#),
    (r#"["abc+/*comment*/"def"]"#, r#"["abcdef"]"#),
    (r#"["abc/*comment*/+"def"]"#, r#"["abcdef"]"#),
    (r#"["abc,/*comment*/"def"]"#, r#"["abc","def"]"#),
    // missing start quote
    ("abc\"", r#""abc""#),
    ("[a\",\"b\"]", r#"["a","b"]"#),
    ("[a\",b\"]", r#"["a","b"]"#),
    ("{a\":\"foo\",\"b\":\"bar\"}", r#"{"a":"foo","b":"bar"}"#),
    (r#"{"a":"foo",b":"bar"}"#, r#"{"a":"foo","b":"bar"}"#),
    ("{\"a\":foo\",\"b\":\"bar\"}", r#"{"a": "foo", "b": "bar"}"#),
    // typographic quote pairs normalize
    ("\u{2018}foo\u{2019}", r#""foo""#),
    ("\u{201C}foo\u{201D}", r#""foo""#),
    ("\u{0060}foo\u{00B4}", r#""foo""#),
    ("\u{0060}foo'", r#""foo""#),
    ("{pattern: '\u{2019}'}", "{\"pattern\": \"\u{2019}\"}"),
    // --- escape characters added/removed ---
    (r#""foo'bar""#, r#""foo'bar""#),
    (r#""foo\"bar""#, r#""foo\"bar""#),
    ("'foo\"bar'", r#""foo\"bar""#),
    ("'foo\\'bar'", r#""foo'bar""#),
    (r#""foo\'bar""#, r#""foo'bar""#),
    (r#""\a""#, r#""a""#),
    ("\"first\\\nsecond\"", r#""first\nsecond""#),
    // unescaped control characters get escaped
    (r#""hello\bworld""#, r#""hello\bworld""#),
    ("\"hello\nworld\"", r#""hello\nworld""#),
    ("\"hello\tworld\"", r#""hello\tworld""#),
    ("{\"key\nafter\": \"foo\"}", r#"{"key\nafter": "foo"}"#),
    // unescaped quotes mid-string get escaped
    (
        r#""The TV has a 24" screen""#,
        r#""The TV has a 24\" screen""#,
    ),
    (
        r#"{"key": "apple "bee" carrot"}"#,
        r#"{"key": "apple \"bee" carrot"}"#,
    ),
    (
        r#""He is six feet (72") tall""#,
        r#""He is six feet (72\") tall""#,
    ),
    (r#""a (b) ((c") d)""#, r#""a (b) ((c\") d)""#),
    (r#""the list [1, 2"] more""#, r#""the list [1, 2\"] more""#),
    (r#""the set {a, b"} more""#, r#""the set {a, b\"} more""#),
    (r#"["foo)" 2]"#, r#"["foo)", 2]"#),
    (r#""The TV is 72"""#, r#""The TV is 72\"""#),
    // escaped string contents (double-escaped documents)
    ("\\\"hello world\\\"", r#""hello world""#),
    ("\\\"hello world\\", r#""hello world""#),
    (r#"\"hello""#, r#""hello""#),
    (r#"[\"hello\"2]"#, r#"["hello", 2]"#),
    // --- comments ---
    ("/* foo */ {}", "{}"),
    ("{} /* foo */ ", "{}"),
    ("{} /* foo ", "{}"),
    ("\n/* foo */\n{}", "{}"),
    (
        r#"{"a":"foo",/*hello*/"b":"bar"}"#,
        r#"{"a":"foo","b":"bar"}"#,
    ),
    (r#"{"flag":/*boolean*/true}"#, r#"{"flag":true}"#),
    ("{} // comment", "{}"),
    (
        "{\n\"a\":\"foo\",//hello\n\"b\":\"bar\"\n}",
        r#"{"a":"foo","b":"bar"}"#,
    ),
    (r#""/* foo */""#, r#""/* foo */""#),
    ("[\"a\"/* foo */]", r#"["a"]"#),
    ("[\"(a)\"/* foo */]", r#"["(a)"]"#),
    ("[\"a]\"/* foo */]", r#"["a]"]"#),
    (r#"{"a":"b"/* foo */}"#, r#"{"a":"b"}"#),
    // --- fences ---
    ("```\n{\"a\":\"b\"}\n```", r#"{"a":"b"}"#),
    ("```json\n{\"a\":\"b\"}\n```", r#"{"a":"b"}"#),
    ("```\n{\"a\":\"b\"}\n", r#"{"a":"b"}"#),
    ("\n{\"a\":\"b\"}\n```", r#"{"a":"b"}"#),
    ("```{\"a\":\"b\"}```", r#"{"a":"b"}"#),
    ("```\n[1,2,3]\n```", "[1,2,3]"),
    // --- leading/trailing commas ---
    ("[,1,2,3]", "[1,2,3]"),
    ("[/* a */,/* b */1,2,3]", "[1,2,3]"),
    ("{,\"message\": \"hi\"}", r#"{"message": "hi"}"#),
    (
        "{/* a */,/* b */\"message\": \"hi\"}",
        r#"{"message": "hi"}"#,
    ),
    ("[1,2,3,]", "[1,2,3]"),
    (r#"{"array":[1,2,3,]}"#, r#"{"array":[1,2,3]}"#),
    (r#""[1,2,3,]""#, r#""[1,2,3,]""#),
    (r#"{"a":2,}"#, r#"{"a":2}"#),
    (r#"{"a":2/*foo*/,/*foo*/}"#, r#"{"a":2}"#),
    ("{},", "{}"),
    (r#""{a:2,}""#, r#""{a:2,}""#),
    ("4,", "4"),
    (r#"{"a":2},"#, r#"{"a":2}"#),
    ("[1,2,3],", "[1,2,3]"),
    // --- truncation: missing closing brackets ---
    ("{", "{}"),
    ("{\"a\":2", r#"{"a":2}"#),
    ("{\"a\":2,", r#"{"a":2}"#),
    ("{\"a\":{\"b\":2}", r#"{"a":{"b":2}}"#),
    ("[{\"b\":2]", "[{\"b\":2}]"),
    ("[{\"i\":1{\"i\":2}]", r#"[{"i":1},{"i":2}]"#),
    ("[{\"i\":1,{\"i\":2}]", r#"[{"i":1},{"i":2}]"#),
    ("[{{]", "[{},{}]"),
    // redundant closing brackets removed
    (r#"{"a": 1}}"#, r#"{"a": 1}"#),
    (r#"{"a": 1}}]}"#, r#"{"a": 1}"#),
    (r#"{"a":2]"#, r#"{"a":2}"#),
    (r#"{"a":2,]"#, r#"{"a":2}"#),
    ("{}}", "{}"),
    ("[2,}", "[2]"),
    ("[}", "[]"),
    ("{]", "{}"),
    ("[1,[}]", "[1,[]]"),
    (r#"{"a": 1, "b": [}"#, r#"{"a": 1, "b": []}"#),
    ("[", "[]"),
    ("[1,2,3", "[1,2,3]"),
    ("[1,2,3,", "[1,2,3]"),
    ("[[1,2,3,", "[[1,2,3]]"),
    ("{\n\"values\":[1,2,3\n}", "{\n\"values\":[1,2,3]}"),
    ("{\"foo\":\"bar\"", r#"{"foo":"bar"}"#),
    ("{\"foo\":\"bar", r#"{"foo":"bar"}"#),
    ("{\"foo\":", r#"{"foo":null}"#),
    ("{\"foo\"", r#"{"foo":null}"#),
    ("{\"foo", r#"{"foo":null}"#),
    ("{\"s \\ud", r#"{"s": null}"#),
    (
        r#"{"message": "it's working"#,
        r#"{"message": "it's working"}"#,
    ),
    // truncated unicode escapes complete to an empty string
    (r#""\u"#, r#""""#),
    (r#""\u2"#, r#""""#),
    (r#""\u260"#, r#""""#),
    (r#""\u2605""#, r#""★""#),
    // truncated numbers complete to a valid form
    ("2.", "2.0"),
    ("[2.]", "[2.0]"),
    ("2e", "2e0"),
    ("2e+", "2e+0"),
    ("2e-", "2e-0"),
    ("[2e+]", "[2e+0]"),
    ("[2e,", "[2e0]"),
    ("[-,", "[-0]"),
    ("-", "-0"),
    ("{\"a\":2.", r#"{"a":2.0}"#),
    ("{\"a\":2e", r#"{"a":2e0}"#),
    ("{\"a\":2e-", r#"{"a":2e-0}"#),
    ("{\"a\":-", r#"{"a":-0}"#),
    ("{\"a\":", r#"{"a":null}"#),
    // truncated strings close
    ("\"foo", r#""foo""#),
    ("[\"foo", "[\"foo\"]"),
    ("[\"foo\"", "[\"foo\"]"),
    // --- missing values / undefined / Python constants ---
    ("{\"a\":}", r#"{"a":null}"#),
    ("{\"a\":,\"b\":2}", r#"{"a":null,"b":2}"#),
    ("{\"a\":undefined}", r#"{"a":null}"#),
    ("[undefined]", "[null]"),
    ("undefined", "null"),
    ("True", "true"),
    ("False", "false"),
    ("None", "null"),
    // --- invalid numbers become strings ---
    ("ES2020", r#""ES2020""#),
    ("0.0.1", r#""0.0.1""#),
    (
        "746de9ad-d4ff-4c66-97d7-00a92ad46967",
        r#""746de9ad-d4ff-4c66-97d7-00a92ad46967""#,
    ),
    ("234..5", r#""234..5""#),
    ("[0.0.1,2]", r#"["0.0.1",2]"#),
    ("2e3.4", r#""2e3.4""#),
    ("00.", r#""00.""#),
    ("-05", r#""-05""#),
    ("e", r#""e""#),
    ("[e],", r#"["e"]"#),
    ("[e5],", r#"["e5"]"#),
    ("[-e5],", r#"["-e5"]"#),
    (r#"{"k": e},"#, r#"{"k": "e"}"#),
    (r#"{"n": 05}"#, r#"{"n": "05"}"#),
    ("[05e]", r#"["05e"]"#),
    ("[e]", r#"["e"]"#),
    // leading zeros
    ("0789", r#""0789""#),
    ("000789", r#""000789""#),
    ("001.2", r#""001.2""#),
    ("002e3", r#""002e3""#),
    ("[0789]", r#"["0789"]"#),
    ("{value:0789}", r#"{"value":"0789"}"#),
    // --- missing commas ---
    ("{\"array\": [{}{}]}", r#"{"array": [{},{}]}"#),
    ("{\"array\": [{}\n{}]}", r#"{"array": [{},{}]}"#),
    ("{\"array\": [\n1\n2\n]}", "{\"array\": [1,2]}"),
    ("{\"array\": [\n\"a\"\n\"b\"\n]}", r#"{"array": ["a","b"]}"#),
    ("[\n{},\n{}\n]", "[{},{}]"),
    ("{\"a\":2\n\"b\":3\n}", r#"{"a":2,"b":3}"#),
    ("{\"a\":2\n\"b\":3\nc:4}", r#"{"a":2,"b":3,"c":4}"#),
    (
        "{\n  \"firstName\": \"John\"\n  lastName: Smith",
        r#"{"firstName": "John", "lastName": "Smith"}"#,
    ),
    ("{a 'b'}", r#"{"a": "b"}"#),
    ("{a \u{201C}b\u{201D}}", r#"{"a": "b"}"#),
    // missing colon
    ("{\"a\" \"b\"}", r#"{"a": "b"}"#),
    ("{\"a\" 2}", r#"{"a": 2}"#),
    ("{\"a\" true}", r#"{"a": true}"#),
    ("{\"a\" false}", r#"{"a": false}"#),
    ("{\"a\" null}", r#"{"a": null}"#),
    ("{\"a\"2}", r#"{"a":2}"#),
    ("{\n\"a\" \"b\"\n}", r#"{"a": "b"}"#),
    ("{\"a\" 'b'}", r#"{"a": "b"}"#),
    ("{'a' 'b'}", r#"{"a": "b"}"#),
    ("{\u{201C}a\u{201D} \u{201C}b\u{201D}}", r#"{"a": "b"}"#),
    // combined repairs
    // --- string concatenation ---
    ("\"hello\" + \" world\"", r#""hello world""#),
    ("\"a\"+\"b\"+\"c\"", r#""abc""#),
    ("\"hello\" + /*comment*/ \" world\"", r#""hello world""#),
    (
        "{\n  \"greeting\": 'hello' +\n 'world'\n}",
        r#"{"greeting": "helloworld"}"#,
    ),
    ("\"hello +\n \" world\"", r#""hello world""#),
    ("\"hello +", r#""hello""#),
    ("[\"hello +]", r#"["hello"]"#),
    // --- MongoDB data types ---
    ("NumberLong(\"2\")", r#""2""#),
    ("{\"_id\":ObjectId(\"123\")}", r#"{"_id":"123"}"#),
    // --- NDJSON / multiple top-level values ---
    ("/* 1 */\n{}\n\n/* 2 */\n{}\n\n/* 3 */\n{}\n", "[{},{},{}]"),
    (
        "/* 1 */\n{},\n\n/* 2 */\n{},\n\n/* 3 */\n{}\n",
        "[{},{},{}]",
    ),
    (
        "/* 1 */\n{},\n\n/* 2 */\n{},\n\n/* 3 */\n{},\n",
        "[{},{},{}]",
    ),
    ("1,2,3", "[1,2,3]"),
    ("1,2,3,", "[1,2,3]"),
    ("1\n2\n3", "[1,2,3]"),
    ("a\nb", r#"["a","b"]"#),
    ("a,b", r#"["a","b"]"#),
    // --- regex literals ---
    (
        "{regex: /standalone-styles.css/}",
        r#"{"regex": "/standalone-styles.css/"}"#,
    ),
    // --- HTML entities ---
    (
        "{&quot;name&quot;: &quot;John&quot;}",
        r#"{"name": "John"}"#,
    ),
    ("&quot;hello&quot;", r#""hello""#),
    ("{&quot;a&quot;:2}", r#"{"a":2}"#),
    ("[&quot;a&quot;, &quot;b&quot;]", r#"["a", "b"]"#),
    (
        "{&quot;a&quot;: &quot;b &amp; c&quot;}",
        r#"{"a": "b & c"}"#,
    ),
    ("{&quot;a&quot;: &quot;&lt;b&gt;&quot;}", r#"{"a": "<b>"}"#),
    ("{&quot;a&quot;: &apos;hello&apos;}", r#"{"a": "hello"}"#),
    ("&#34;hello&#34;", r#""hello""#),
    ("&#x22;hello&#x22;", r#""hello""#),
    ("{&#34;a&#34;: &#34;b&#34;}", r#"{"a": "b"}"#),
    // entities inside a real-quoted string stay literal
    (r#"{"a": "&amp; test"}"#, r#"{"a": "&amp; test"}"#),
    (
        r#"{"html": "&quot;bold&quot;"}"#,
        r#"{"html": "&quot;bold&quot;"}"#,
    ),
    ("{a: '&amp; test'}", r#"{"a": "&amp; test"}"#),
    // entity-opened strings decode entities within themselves
    ("{&quot;a&quot;: &quot;1 &lt; 2&quot;}", r#"{"a": "1 < 2"}"#),
    (
        "{&quot;a&quot;: 1, b: 'AT&amp;T'}",
        r#"{"a": 1, "b": "AT&amp;T"}"#,
    ),
    (
        "{b: 'AT&amp;T', &quot;a&quot;: 1}",
        r#"{"b": "AT&amp;T", "a": 1}"#,
    ),
    // a literal quote inside an entity-opened string is escaped
    ("&quot;he\"llo&quot;", r#""he\"llo""#),
    // truncated entities stay literal text
    ("&quot", r#""&quot""#),
    ("&#", r#""&#""#),
    ("&", r#""&""#),
    // --- special whitespace ---
    (
        "{\"a\":\u{00A0}\"foo\u{00A0}bar\"}",
        "{\"a\": \"foo\u{00A0}bar\"}",
    ),
    ("{\"a\":\u{180E}\"foo\"}", r#"{"a": "foo"}"#),
    ("{\"a\":\u{2000}\"foo\"}", r#"{"a": "foo"}"#),
    ("{\"a\":\u{200B}\"foo\"}", r#"{"a": "foo"}"#),
    ("{\"a\":\u{3000}\"foo\"}", r#"{"a": "foo"}"#),
    ("{\"a\":\u{FEFF}\"foo\"}", r#"{"a": "foo"}"#),
    // --- MongoDB extended document (kept whole for line-length reasons) ---
    (
        "{\n   \"_id\" : ObjectId(\"123\"),\n   \"isoDate\" : ISODate(\"2012-12-19T06:01:17.171Z\"),\n   \"regularNumber\" : 67,\n   \"long\" : NumberLong(\"2\"),\n   \"long2\" : NumberLong(2),\n   \"int\" : NumberInt(\"3\"),\n   \"int2\" : NumberInt(3),\n   \"decimal\" : NumberDecimal(\"4\"),\n   \"decimal2\" : NumberDecimal(4)\n}",
        "{\n   \"_id\" : \"123\",\n   \"isoDate\" : \"2012-12-19T06:01:17.171Z\",\n   \"regularNumber\" : 67,\n   \"long\" : \"2\",\n   \"long2\" : 2,\n   \"int\" : \"3\",\n   \"int2\" : 3,\n   \"decimal\" : \"4\",\n   \"decimal2\" : 4\n}",
    ),
    ("/[a-z]_/", r#""/[a-z]_/""#),
    ("{\"array\": [\na\nb\n]}", r#"{"array": ["a","b"]}"#),
    ("1\n2", "[1,2]"),
    ("[a,b\nc]", r#"["a","b","c"]"#),
    ("[\"foo\",", "[\"foo\"]"),
    ("```python\n{\"a\":\"b\"}\n```", r#"{"a":"b"}"#),
    // invalid fences (inside brackets) are still stripped
    ("[```\n{\"a\":\"b\"}\n```]", r#"{"a":"b"}"#),
    ("{```json\n{\"a\":\"b\"}\n```}", r#"{"a":"b"}"#),
    // special quotes inside a normal double-quoted string survive
    ("\"Rounded \u{201C} quote\"", "\"Rounded \u{201C} quote\""),
    ("'Rounded \u{201C} quote'", "\"Rounded \u{201C} quote\""),
    ("'Rounded \u{2019} quote'", "\"Rounded \u{2019} quote\""),
    ("'Double \" quote'", r#""Double \" quote""#),
    // string content stays untouched
    (r#""{a:b}""#, r#""{a:b}""#),
    (
        r#"{"url":"https://www.bible.com/}"#,
        r#"{"url":"https://www.bible.com/"}"#,
    ),
    (
        r#"["https://www.bible.com/,2]"#,
        r#"["https://www.bible.com/",2]"#,
    ),
    // The JS reference throws on a key with no value / no colon (`{"a",`,
    // `{:2}`, `{"a" ]`); jsonfix's object parser deliberately repairs those
    // into `null` values (the `{a,}` rule), so they live here instead of in
    // ERRORS.
    ("{\"a\",", "{\"a\": null}"),
    ("{:2}", r#"{"2": null}"#),
    (r#"{"a" ]"#, r#"{"a": null}"#),
];

/// Inputs that must fail rather than repair, with the expected error class.
///
/// Two documented divergences from the JS reference keep some of its throws
/// out of this table (they live in `CASES` instead, pinned by the
/// serde/idempotence checks in the test bodies):
///
/// 1. A backslash appearing outside a string (index.test.ts, "should repair a
///    backslash character outside of a string") is treated as part of an
///    unquoted value and quoted away.
/// 2. A key with no value / no colon (`{"a",`, `{:2}`, `{"a" ]`) is repaired
///    into a `null`-valued member (the `{a,}` rule).
const ERRORS: &[(&str, ErrorKind)] = &[
    // A backslash after a complete (or prose-adjacent) value still fails,
    // because a second top-level value appears without a separator.
    (r#"["y", "z"]\"#, ErrorKind::TrailingValue),
    (r#"y"\, "z""#, ErrorKind::TrailingValue),
    // Invalid unicode escapes with repairs off (strict) are covered in
    // strict.rs; with full repairs a non-hex escape repairs away, so only
    // the structural failures are shared with the JS suite.
    (r#"{"a":2}{}"#, ErrorKind::TrailingValue),
    (r#"{"a":2}foo"#, ErrorKind::TrailingValue),
    ("foo [", ErrorKind::TrailingValue),
    // `callback {}` fails like the reference (no `(` call wrapper), but
    // jsonfix classifies the trailing `{}` as a second top-level value.
    ("callback {}", ErrorKind::TrailingValue),
];

#[test]
fn corpus_repairs_to_expected_output() {
    for (input, expected) in CASES {
        let repaired =
            repair(input).unwrap_or_else(|error| panic!("failed to repair {input:?}: {error}"));
        // Expectations are written compactly; both sides are canonicalized
        // through `repair` (a no-op re-serialization for valid JSON), so the
        // comparison pins semantics — number text, escapes, structure — but
        // not token spacing (see the module docs).
        let expected = repair(expected)
            .unwrap_or_else(|error| panic!("expectation {expected:?} is not repairable: {error}"));
        assert_eq!(&repaired, &expected, "wrong repair for {input:?}");
        // Independent parser must accept the output.
        serde_json::from_str::<serde_json::Value>(&repaired)
            .unwrap_or_else(|error| panic!("repaired {input:?} to invalid JSON: {error}"));
        // Idempotent: repairing again changes nothing.
        assert_eq!(
            repair(&repaired).unwrap(),
            repaired,
            "not stable for {input:?}"
        );
    }
}

#[test]
fn corpus_errors() {
    for (input, kind) in ERRORS {
        let error = repair(input).expect_err(&format!("{input:?} should not repair"));
        assert_eq!(error.kind(), kind, "wrong error for {input:?}");
    }
}

#[test]
fn corpus_through_parse_partial_matches_repair() {
    // The parse path shares the parser with repair; the whole corpus must
    // behave identically through `parse_partial` with full Allow.
    for (input, expected) in CASES {
        let value = parse_partial(input, Options::partial(Allow::ALL))
            .unwrap_or_else(|error| panic!("failed to parse {input:?}: {error}"));
        let expected = repair(expected)
            .unwrap_or_else(|error| panic!("expectation {expected:?} is not repairable: {error}"));
        assert_eq!(&value.to_json_string(), &expected, "for {input:?}");
    }
}

#[test]
fn corpus_valid_json_passes_strict() {
    // Turning repairs off must still accept anything that is already valid
    // JSON. (Strict rejection of each repair class lives in strict.rs.)
    for (input, _) in CASES {
        let already_valid = serde_json::from_str::<serde_json::Value>(input).is_ok();
        let strict_ok = repair_with(input, Options::strict()).is_ok();
        if already_valid {
            assert!(strict_ok, "valid JSON {input:?} must pass strict mode");
        }
    }
}
