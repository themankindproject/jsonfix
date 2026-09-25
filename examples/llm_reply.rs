//! Repair a model reply, then read fields out of it without `serde`.
//!
//! Run with: `cargo run --example llm_reply`

use jsonfix::{extract, parse, repair_extract};

fn main() {
    let reply = "\
Sure! Here is the structured result:

```json
{
  'title': 'Quarterly report',
  'tags': ['finance', 'q3',],
  'total': 0001234.50,
  'reviewed': True,
}
```

Let me know if you want any changes.";

    println!("extracted : {}", extract(reply).unwrap_or("<none>"));

    // Prose goes through `repair_extract`: `repair` only accepts a bare
    // document (or newline-separated values) and rejects surrounding text.
    let repaired = repair_extract(reply).expect("repairable");
    println!("repaired  : {repaired}");

    let value = parse(&repaired).expect("parsable");
    println!(
        "title     : {}",
        value.get("title").and_then(|v| v.as_str()).unwrap_or("?")
    );
    println!(
        "tags      : {}",
        value.get("tags").map(|v| v.len()).unwrap_or(0)
    );
    println!(
        "pointer   : {}",
        value
            .pointer("/tags/1")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
    );
    // Numbers keep their text: `0001234.50` cannot be a JSON number, so it is
    // kept as the string "0001234.50".
    println!(
        "total     : {}",
        value
            .get("total")
            .map(|v| v.to_string())
            .unwrap_or_default()
    );
}
