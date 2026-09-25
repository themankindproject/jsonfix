//! Benchmarks for the three hot paths: repair, partial parsing, and extraction.

use core::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use jsonfix::{
    Allow, Options, extract, from_value, parse, parse_partial, repair, repair_bytes,
    repair_bytes_into, repair_extract,
};

/// A reply shaped like real model output: fences, prose, and repairs needed.
fn llm_reply() -> String {
    let mut out = String::from("Sure! Here is the plan:\n```json\n{\n  'title': 'Ship jsonfix',\n");
    for index in 0..200 {
        out.push_str(&format!(
            "  \"step_{index}\": {{\"done\": {}, \"weight\": {index}.5, \"note\": \"step {index}\"}},\n",
            index % 2 == 0
        ));
    }
    out.push_str("}\n```\nLet me know if you want changes.");
    out
}

/// The fenced body alone — what `extract` pulls out and `repair` consumes.
/// (Raw prose must go through `extract*`; `repair` rejects it by design.)
fn llm_body() -> String {
    extract(&llm_reply()).expect("fenced body").to_string()
}

/// The body without its tail, as a stream would deliver it mid-object.
fn truncated_body() -> String {
    let full = llm_body();
    let cut = full.len() * 3 / 5;
    let cut = (0..=cut)
        .rev()
        .find(|index| full.is_char_boundary(*index))
        .unwrap_or(0);
    String::from(&full[..cut])
}

fn bench_repair(c: &mut Criterion) {
    let reply = llm_reply();
    let body = llm_body();
    let mut group = c.benchmark_group("repair");
    group.throughput(Throughput::Bytes(body.len() as u64));
    group.bench_function("fenced_body", |b| {
        b.iter(|| repair(black_box(&body)).expect("repairable"));
    });
    group.bench_function("extract_then_repair", |b| {
        b.iter(|| {
            let span = extract(black_box(&reply)).unwrap_or(&reply);
            repair(black_box(span)).expect("repairable")
        });
    });
    group.bench_function("repair_extract", |b| {
        b.iter(|| repair_extract(black_box(&reply)).expect("repairable"));
    });
    group.finish();
}

fn bench_partial(c: &mut Criterion) {
    let truncated = truncated_body();
    let opts = Options::partial(Allow::OBJ | Allow::STR | Allow::NUM | Allow::KEY);
    let mut group = c.benchmark_group("partial");
    group.throughput(Throughput::Bytes(truncated.len() as u64));
    group.bench_function("parse_partial", |b| {
        b.iter(|| parse_partial(black_box(&truncated), opts).expect("repairable"));
    });
    group.bench_function("parse_full", |b| {
        b.iter(|| parse(black_box(&truncated)).expect("repairable"));
    });
    group.finish();
}

fn bench_extract(c: &mut Criterion) {
    let reply = llm_reply();
    let mut group = c.benchmark_group("extract");
    group.throughput(Throughput::Bytes(reply.len() as u64));
    group.bench_function("fenced", |b| {
        b.iter(|| extract(black_box(&reply)).expect("has a value"));
    });
    group.finish();
}

/// Byte-buffer repair (alloc-only entry points) against the text path: the
/// byte API must not cost measurably more than `repair` on the same input.
fn bench_bytes(c: &mut Criterion) {
    let body = llm_body();
    let input = body.as_bytes();
    let mut group = c.benchmark_group("bytes");
    group.throughput(Throughput::Bytes(input.len() as u64));
    group.bench_function("repair", |b| {
        b.iter(|| repair_bytes(black_box(input)).expect("repairable"));
    });
    group.bench_function("repair_bytes_into", |b| {
        let mut sink = Vec::with_capacity(body.len());
        b.iter(|| {
            sink.clear();
            repair_bytes_into(black_box(input), &mut sink, Options::all()).expect("repairable");
            black_box(&sink);
        });
    });
    group.finish();
}

/// Typed deserialization from the repaired tree (`from_value`): parse once
/// per iteration, then read the tree into a struct — the typed LLM-reply
/// path with no intermediate `String` and no `serde_json`.
fn bench_from_value(c: &mut Criterion) {
    // The fields define the deserialization shape — that is the measured
    // work, so they are deliberately never read afterwards.
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Step {
        done: bool,
        weight: f32,
    }
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Plan {
        title: String,
        #[serde(flatten)]
        steps: std::collections::BTreeMap<String, Step>,
    }

    let body = llm_body();
    let value = parse(&body).expect("repairable");
    let mut group = c.benchmark_group("from_value");
    group.throughput(Throughput::Bytes(body.len() as u64));
    group.bench_function("parse_then_from_value", |b| {
        b.iter(|| {
            let value = parse(black_box(&body)).expect("repairable");
            from_value::<Plan>(value).expect("typed")
        });
    });
    group.bench_function("from_value_only", |b| {
        b.iter(|| from_value::<Plan>(black_box(value.clone())).expect("typed"));
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_repair,
    bench_partial,
    bench_extract,
    bench_bytes,
    bench_from_value
);
criterion_main!(benches);
