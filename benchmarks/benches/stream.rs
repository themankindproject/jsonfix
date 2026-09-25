//! Streaming benchmarks: total time to feed a document in many chunks.
//!
//! `bench_stream_token_chunks` is the O(N^2) canary: per-push work must scale
//! with the appended tail, not with the accumulated input (total ≈ linear).

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use jsonfix::StreamRepairer;

/// A flat object with many members — the common LLM streaming shape.
fn flat_object(members: usize) -> String {
    let mut out = String::from("{\"title\": \"Ship jsonfix\",");
    for i in 0..members {
        out.push_str(&format!(
            "\"step_{i}\": {{\"done\": {}, \"weight\": {i}.5, \"note\": \"step {i}\"}},",
            i % 2 == 0
        ));
    }
    out.push_str("\"end\"}");
    out
}

/// NDJSON: many independent top-level values, one per line.
fn ndjson(lines: usize) -> String {
    let mut out = String::new();
    for i in 0..lines {
        out.push_str(&format!("{{\"n\": {i}, \"tag\": \"line-{i}\"}}\n"));
    }
    out
}

/// Splits into ~chunk_size pieces on char boundaries.
fn chunks(input: &str, chunk_size: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < input.len() {
        let mut end = (start + chunk_size).min(input.len());
        while end > start && !input.is_char_boundary(end) {
            end -= 1;
        }
        out.push(input[start..end].to_string());
        start = end;
    }
    out
}

fn bench_stream_chunks(c: &mut Criterion) {
    let doc = flat_object(200); // ~11 KB, 200 members
    let parts = chunks(&doc, 64);
    let mut group = c.benchmark_group("stream/chunks_64b");
    group.throughput(Throughput::Bytes(doc.len() as u64));
    group.bench_function("flat_object_200", |b| {
        b.iter_custom(|iters| {
            let t = Instant::now();
            for _ in 0..iters {
                let mut stream = StreamRepairer::new();
                for part in &parts {
                    black_box(stream.push(black_box(part)).expect("repairs"));
                }
                black_box(stream.output());
            }
            t.elapsed()
        });
    });
    group.finish();
}

fn bench_stream_ndjson(c: &mut Criterion) {
    let doc = ndjson(200);
    let lines: Vec<String> = doc.lines().map(|l| format!("{l}\n")).collect();
    let mut group = c.benchmark_group("stream/ndjson_lines");
    group.throughput(Throughput::Bytes(doc.len() as u64));
    group.bench_function("200_lines", |b| {
        b.iter_custom(|iters| {
            let t = Instant::now();
            for _ in 0..iters {
                let mut stream = StreamRepairer::new();
                for line in &lines {
                    black_box(stream.push(black_box(line)).expect("repairs"));
                }
                black_box(stream.output());
            }
            t.elapsed()
        });
    });
    group.finish();
}

fn bench_stream_grow(c: &mut Criterion) {
    // One value growing one character at a time (worst case: no stable
    // checkpoint can advance mid-value) on a SMALL doc so the O(N^2)
    // baseline stays benchable.
    let doc = "{\"items\": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10], \"ok\": true}";
    let mut group = c.benchmark_group("stream/one_char_at_a_time");
    group.throughput(Throughput::Bytes(doc.len() as u64));
    group.bench_function("small_doc", |b| {
        b.iter_custom(|iters| {
            let t = Instant::now();
            for _ in 0..iters {
                let mut stream = StreamRepairer::new();
                for ch in doc.chars() {
                    black_box(stream.push(&ch.to_string()).expect("repairs"));
                }
                black_box(stream.output());
            }
            t.elapsed()
        });
    });
    group.finish();
}

fn bench_stream_token_chunks(c: &mut Criterion) {
    // LLM token streams: a large document delivered in tiny chunks, the
    // shape where a per-push rescan of the accumulated input shows up as
    // O(N^2) total. This must stay ~linear in total input size.
    let doc = flat_object(800); // ~40 KB
    let parts = chunks(&doc, 8);
    let mut group = c.benchmark_group("stream/token_chunks_8b");
    group.throughput(Throughput::Bytes(doc.len() as u64));
    group.bench_function("flat_object_800", |b| {
        b.iter_custom(|iters| {
            let t = Instant::now();
            for _ in 0..iters {
                let mut stream = StreamRepairer::new();
                for part in &parts {
                    black_box(stream.push(black_box(part)).expect("repairs"));
                }
                black_box(stream.output());
            }
            t.elapsed()
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_stream_chunks,
    bench_stream_ndjson,
    bench_stream_grow,
    bench_stream_token_chunks
);
criterion_main!(benches);
