# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/isacgalvao/jdk/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.1.0
