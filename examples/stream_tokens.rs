//! Repair JSON while it streams in token by token, the way a chat UI does.
//!
//! Run with: `cargo run --example stream_tokens`

use jsonfix::{Allow, Options, StreamRepairer, parse_partial};

fn main() {
    let tokens = [
        "{\"answer\": ",
        "\"Yes — here ",
        "is the list:\",",
        " \"items\": [\"a\", ",
        "\"b\"], \"count\": 2",
    ];

    // 1. Full repair after every chunk: render the whole document each time.
    let mut stream = StreamRepairer::new();
    for token in tokens {
        let rendered = stream.push(token).expect("repairable");
        println!("render: {rendered}");
    }

    // 2. Deltas: keep only what changed.
    let mut stream = StreamRepairer::new();
    let mut buffer = String::new();
    for token in tokens {
        let delta = stream.push_delta(token).expect("repairable");
        buffer.truncate(delta.keep);
        buffer.push_str(delta.text);
    }
    assert_eq!(buffer, stream.output());
    println!("delta : {buffer}");

    // 3. Partial values: read a field before the document is finished.
    let opts = Options::partial(Allow::ALL);
    let partial = parse_partial("{\"answer\": \"still writ", opts).expect("repairable");
    println!(
        "early : {}",
        partial
            .get("answer")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
    );
}
