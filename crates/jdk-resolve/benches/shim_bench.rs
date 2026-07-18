//! Measurement harness for the shim's per-invocation overhead budget: what
//! a `java` call pays before the real JVM is spawned. Not a CI gate — run
//! manually with `cargo bench -p jdk-resolve`.

use criterion::{Criterion, criterion_group, criterion_main};
use jdk_resolve::selector::Selector;
use jdk_resolve::{cascade, config, store};
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use tempfile::TempDir;

/// What one shim invocation sees: a store with a few candidates plus a
/// config, and a project pinning java at its root, resolved from a deep
/// package-style subdirectory.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("store");
    for name in [
        "temurin@17.0.9",
        "temurin@21.0.4",
        "temurin@21.0.5+11",
        "zulu@21.0.3",
        "corretto@21.0.7.6.1",
    ] {
        fs::create_dir_all(store::java_candidates(&root).join(name)).unwrap();
    }
    fs::write(
        store::config(&root),
        "vendor = \"temurin\"\nauto-install = \"prompt\"\n",
    )
    .unwrap();

    let project = temp.path().join("proj");
    let deep = project
        .join("src")
        .join("main")
        .join("java")
        .join("com")
        .join("acme")
        .join("app");
    fs::create_dir_all(&deep).unwrap();
    fs::write(project.join(".sdkmanrc"), "java=21.0.4-tem\n").unwrap();

    (temp, root, deep)
}

fn bench_shim_resolution(c: &mut Criterion) {
    let (_guard, root, deep) = fixture();

    c.bench_function("cascade_from_deep_subdirectory", |b| {
        b.iter(|| black_box(cascade::resolve(black_box(&deep)).unwrap()))
    });

    c.bench_function("store_best_candidate", |b| {
        let selector: Selector = "temurin@21".parse().unwrap();
        b.iter(|| black_box(store::best_candidate(black_box(&root), &selector, "temurin").unwrap()))
    });

    // The complete pre-spawn path of one shim run: config, cascade, match.
    c.bench_function("shim_resolution_end_to_end", |b| {
        b.iter(|| {
            let config = config::load(black_box(&root)).unwrap();
            let resolution = cascade::resolve(black_box(&deep)).unwrap();
            let pin = resolution.pin.expect("fixture pins java");
            black_box(
                store::best_candidate(black_box(&root), &pin.selector, &config.vendor).unwrap(),
            )
        })
    });
}

criterion_group!(benches, bench_shim_resolution);
criterion_main!(benches);
