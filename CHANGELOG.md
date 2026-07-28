# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-07-28

A correctness release. Nothing new to do — the things that were already there
now work, and two of them could leave an installation unusable.

### Fixed

- `jdk setup` works after installation. It looked for `jdk-shim.exe` beside the
  running executable, and nothing ever put it there, so the command `doctor` and
  `update` recommend as the remedy failed on every real installation. Both now
  place the shim in `bin` alongside the CLI.
- A `#` in a pre-existing `JAVA_HOME` no longer breaks `java`. Setup saved the
  path to `config.toml`, the reader treated the `#` as a comment even inside
  quotes, and every shim invocation in a pinned directory then exited on a
  config error. The two config readers that disagreed on this became one.
- An interrupted `jdk update` no longer leaves an installation without
  `jdk.exe`. The shim restores the copy that was moved aside, the sweep spares
  it while the executable is missing, and staging leftovers are collected.
- The durability the executable swap documented is now applied: the replace goes
  through `MOVEFILE_WRITE_THROUGH`, which the code claimed and never reached.
- `jdk available --ea` stopped removing general-availability builds — from the
  live catalog, where the early-access query capped both, and from `--latest`,
  where every line that had a GA release hid its early-access build.
- Listing and installing early access agree on what exists. `jdk available`
  implies `--ea` when the filter itself asks for early access — a pre-release
  selector like `temurin@27-ea`, or an explicit version that only exists as an
  early-access build — matching the selectors `jdk install` already resolves.
  And an empty result with early access in play now explains itself, naming
  the vendor — or the whole catalog, when no vendor is named — as publishing
  no early-access builds for the platform.
- A failing early-access query no longer vetoes the whole index publish. One
  vendor's pre-release outage used to take general availability down with it.

### Changed

- **Early-access builds require a pre-release selector.** `jdk install 21.0.12`
  used to silently install `21.0.12-ea+8` whenever the line had no GA release —
  including through the shim's auto-install, inside a build. It now fails and
  names the alternative. Ask for `21.0.12-ea` to get early access.
- **Proprietary licenses need consent.** Installing Oracle JDK or Oracle GraalVM
  printed a notice and downloaded anyway, while the client transmitted Oracle's
  acceptance cookie on the user's behalf. It now prompts, accepts
  `--accept-license` or `accept-license = true` in `config.toml`, and refuses
  otherwise. The config key deliberately does not carry the shim's auto-install:
  a `.jdkrc` comes from a repository, and standing consent is not consent for
  someone else's project to enter a license agreement in your name.
  Unattended installs of these two vendors need one of the two opt-ins.

### Security

- `JDK_RELEASES` is restricted to loopback. It overrode where `jdk update`
  fetches its own replacement, and the checksum came from that same host, so a
  user-writable environment variable could turn one execution into the binary
  every shim invokes.
- The Oracle acceptance cookie is now derived from the license notice instead of
  a separate vendor list. The two had drifted apart in both directions:
  `oracle_open_jdk` transmitted an acceptance cookie through no gate at all, and
  GraalVM collected a GFTC consent that never reached the wire.

### Internal

- The release workflow runs the same gates as CI before it builds or publishes,
  every cargo invocation is `--locked`, and publishing is preceded by a dry run —
  a tag on a commit that never passed CI could previously reach crates.io, and a
  partial publish is unrecoverable.
- The declared MSRV is compiled by CI instead of asserted by a script comparing
  two numbers that were never tested.
- Internal version pins moved to `[workspace.dependencies]`, retiring the
  PowerShell script that policed them.

## [0.4.0] - 2026-07-21

### Added

- `jdk update`: self-update to the latest GitHub release — checksum-verified download, in-process swap of the running `jdk.exe` (the old copy is moved aside and swept on the next run) and shim refresh; `--force` reinstalls that latest release even when it is the one you already run.
- `jdk doctor` now notes when a newer jdk release is available and points at `jdk update`; like the other network probes, being offline is informative and never a failure.

## [0.3.0] - 2026-07-21

### Added

- Early-access builds in the catalog: `jdk available --ea` lists pre-release lines alongside GA (hidden by default), indexed and capped to the latest build of each line so the listing never drowns in nightlies.
- A bare pre-release selector now tracks its moving daily build — `jdk install temurin@27-ea` matches the current `27-ea+N` — and a pinned build like `27-ea+30` resolves exactly while the index still carries it.
- Exact pre-release builds the index no longer carries are resolved live from the foojay Disco API when it publishes an inline sha256 for the vendor — only temurin and zulu do today; every other vendor's expired builds are refused rather than downloaded unverified; `jdk install` reports when a build came from foojay instead of the index.

### Security

- Release artifacts are now signed keyless with cosign (`SHA256SUMS` + `.sigstore.json` bundle) and carry SLSA build-provenance attestations, verifiable with `cosign verify-blob` and `gh attestation verify`.
- The build toolchain is pinned via `rust-toolchain.toml`, and CI enforces a cargo-deny license/advisory policy with Dependabot keeping actions and crates current.

## [0.2.0] - 2026-07-19

### Added

- Oracle JDK as a best-effort vendor — `jdk install oracle@25` — indexed only under its immutable `/archive/` URLs; a bad Oracle day drops it from the daily index with a warning instead of failing the publish. A SHA-256 stays mandatory on every published package, but foojay carries none inline for Oracle: the generator resolves it from Oracle's checksum URI when offered, otherwise pinning the hash it first saw (trust-on-first-use).
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

[Unreleased]: https://github.com/isacgalvao/jdk/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.5.0
[0.4.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.4.0
[0.3.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.3.0
[0.2.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.2.0
[0.1.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.1.0
