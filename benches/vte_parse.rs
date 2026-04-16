//! VTE parser throughput benchmarks.
//!
//! Two built-in corpora:
//!
//! * `parse_ascii_heavy` — mostly printable ASCII with newlines, approximating
//!   the output of `cat bigfile.txt`.
//! * `parse_mixed` — ASCII plus CSI colour runs, occasional UTF-8, and an OSC
//!   title to model an interactive shell session.
//!
//! Set `TERM41_BENCH_CORPUS=<path>` to benchmark against an arbitrary file
//! (e.g. captured output of `ls -laR ~`).

use std::hint::black_box;

use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use vtepp::Parser;

fn ascii_heavy_corpus() -> Vec<u8> {
    // Deterministic: repeat a fixed paragraph until we exceed a PTY read-sized
    // buffer so the SIMD loop has many full-width chunks to grind on.
    let line = b"The quick brown fox jumps over the lazy dog. \
                 Pack my box with five dozen liquor jugs. \
                 Sphinx of black quartz, judge my vow. \
                 How vexingly quick daft zebras jump!\n";
    let mut out = Vec::with_capacity(256 * 1024);
    while out.len() < 256 * 1024 {
        out.extend_from_slice(line);
    }
    out
}

fn mixed_corpus() -> Vec<u8> {
    let mut out = Vec::with_capacity(256 * 1024);
    out.extend_from_slice(b"\x1b]0;bench session\x07");
    while out.len() < 256 * 1024 {
        out.extend_from_slice(b"\x1b[2m$\x1b[0m ls -la\n");
        out.extend_from_slice(b"total 42\n");
        out.extend_from_slice(
            b"\x1b[34mdrwxr-xr-x\x1b[0m  5 user user  4096 Apr 13 21:15 \x1b[1;34msrc\x1b[0m\n",
        );
        out.extend_from_slice(b"-rw-r--r--  1 user user  1024 Apr 13 21:15 README.md\n");
        out.extend_from_slice("café résumé naïve ☃ 🦀\n".as_bytes());
    }
    out
}

fn load_external_corpus() -> Option<(String, Vec<u8>)> {
    let path = std::env::var("TERM41_BENCH_CORPUS").ok()?;
    let bytes = std::fs::read(&path).ok()?;
    Some((path, bytes))
}

fn bench_corpus(
    c: &mut Criterion,
    name: &str,
    data: &[u8],
) {
    let mut group = c.benchmark_group("vte_parse");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_with_input(BenchmarkId::new(name, data.len()), data, |b, data| {
        b.iter(|| {
            let mut parser = Parser::new();
            for action in parser.parse(black_box(data)) {
                black_box(action);
            }
        });
    });
    group.finish();
}

fn bench_all(c: &mut Criterion) {
    let ascii = ascii_heavy_corpus();
    bench_corpus(c, "ascii_heavy", &ascii);

    let mixed = mixed_corpus();
    bench_corpus(c, "mixed", &mixed);

    if let Some((path, bytes)) = load_external_corpus() {
        eprintln!(
            "bench corpus from TERM41_BENCH_CORPUS={} ({} bytes)",
            path,
            bytes.len()
        );
        bench_corpus(c, "external", &bytes);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
