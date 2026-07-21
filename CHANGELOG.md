# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-07-21

### Added

- Early-access builds in the catalog: `jdk available --ea` lists pre-release lines alongside GA (hidden by default), indexed and capped to the latest build of each line so the listing never drowns in nightlies.
- A bare pre-release selector now tracks its moving daily build — `jdk install temurin@27-ea` matches the current `27-ea+N` — while a pinned build like `27-ea+30` still resolves exactly.
- Exact pre-release builds the index no longer carries are resolved live from the foojay Disco API; `jdk install` reports when a build came from foojay instead of the index.

### Security

- Release artifacts are now signed keyless with cosign (`SHA256SUMS` + `.sigstore.json` bundle) and carry SLSA build-provenance attestations, verifiable with `cosign verify-blob` and `gh attestation verify`.
- The build toolchain is pinned via `rust-toolchain.toml`, and CI enforces a cargo-deny license/advisory policy with Dependabot keeping actions and crates current.

## [0.2.0] - 2026-07-19

### Added

- Oracle JDK as an installable vendor — `jdk install oracle@25` — sourced from the foojay Disco API with the same mandatory SHA-256 verification as every other vendor.
- A license notice shown before download for the vendors under proprietary terms: Oracle JDK (NFTC) and Oracle GraalVM (GFTC).

## [0.1.0] - 2026-07-18

First public release — a Windows-first Java version manager.

### Added

- Install, switch and pin JDKs on Windows with no administrator rights.
- Real per-tool `.exe` shims (`java`, `javac`, `jar`, …) that resolve the pinned JDK on every invocation.
- Per-project auto-switch through an upward file cascade — `.jdkrc`, `.sdkmanrc`, `.java-version` and asdf `.tool-versions`, with SDKMAN vendor suffixes understood natively.
- Persistent global `JAVA_HOME` backed by an NTFS junction: `jdk use` retargets it and already-open consoles and IDEs pick up the new JDK on their next call — no reload, no logoff.
- Multi-vendor catalog from the foojay Disco API: Temurin, Zulu, Corretto, Liberica, Microsoft, GraalVM and more.
- Commands: `install`, `uninstall`, `use`, `pin`, `list`, `available`, `current`, `which`, `setup`, `doctor`.
- On-demand auto-install when a project pins a version you don't have, configurable as `prompt`, `always` or `never`.
- PowerShell installer (`install.ps1`) with SHA-256 verification, plus release zips carrying `jdk.exe`, `jdk-shim.exe`, `LICENSE` and `README.md` alongside `.sha256` sidecars.
- Published on crates.io: [`jdk`](https://crates.io/crates/jdk), [`jdk-core`](https://crates.io/crates/jdk-core) and [`jdk-resolve`](https://crates.io/crates/jdk-resolve).

[Unreleased]: https://github.com/isacgalvao/jdk/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.3.0
[0.2.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.2.0
[0.1.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.1.0
