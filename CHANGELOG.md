# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0] - 2026-07-29

A release about proof and reversibility. `jdk update` now refuses anything it
cannot check against a signature this binary carries, and `jdk setup --undo`
gives back the machine setup changed.

### Added

- **`jdk setup --undo`** reverses what setup did: it restores the previous
  `JAVA_HOME` with the registry type it originally had, removes only the PATH
  entries setup added, removes the shims and the junction, and broadcasts the
  change so open consoles see it. Installed JDKs are kept — `--purge` removes
  the store as well. The previous `JAVA_HOME` had been recorded since 0.1.0 for
  exactly this and never read.
- `jdk doctor` checks two things it used to take on faith: that
  `bin\jdk-shim.exe` exists and matches the shims on disk — it is what `jdk
  setup` copies from, so a missing or stale one breaks the documented remedy
  while doctor reports health — and that the published index is one this build
  can read. When `jdk setup` is not the fix, doctor now says what is.
- A bad connection stops looking like a hang. HTTP retries were silent; from the
  second attempt on, each one names what failed and how long the pause will be.

### Changed

- **The live-catalog notice is now only about early access.** Since 0.3.0,
  `jdk install` reported whenever a build came from the foojay API rather than
  the index. It now reports only early-access builds, where the index genuinely
  carries nothing for the selector. A GA release the index has not caught up
  with resolves quietly: that case is ordinary, and the line was landing in the
  middle of every `java` that auto-installed one in CI. The old wording also
  called such a download "unverified against the index", which was never true —
  a sha256 is mandatory on both paths.
- There is a way back from an update, and the tool names it. When `jdk update`
  declines because you are already above the latest release, and after every
  successful update, the message points at `install.ps1 -Version`. `--force`
  now admits in its help that it downgrades when the release is older than what
  you run, instead of describing only reinstallation.
- The catalog index carries a schema version this client enforces. An index
  newer than the client understands is refused with the way forward, rather
  than failing as unparseable JSON — a client can only be protected by a rule
  it already shipped with, which is why this lands while the installed base is
  small enough for it not to matter.

### Fixed

- An update no longer leaves the tools half-replaced in silence. The shims are
  staged and then swapped in two phases, and a tool that refuses is reported —
  every one of them, not just the first to fail.
- The steps of the executable swap that have no fallback are retried under one
  shared deadline, so an update no longer fails outright because a real-time
  scanner held a freshly written `.exe` for a moment.
- `jdk uninstall` stops reporting a full disk as a file in use; only a genuine
  in-use error defers now.
- Failures where Windows most often bites explain themselves: a file held by
  another process names antivirus as the likely cause, and a full disk names
  space.

### Security

- **`jdk update` is anchored to a signature this binary carries.** Every release
  now publishes `SHA256SUMS` and a detached `SHA256SUMS.sig` — an ed25519
  SSHSIG whose public half is compiled into `jdk.exe` — and the updater checks
  it before downloading anything. A release missing either file is refused, so
  "unverified" is no longer a state the release host can put a client into.
  Until now the only check was a `.sha256` sidecar served by whoever served the
  zip, which can prove a transfer but never a release. The signed line names the
  asset by version, so a signature lifted from another release covers nothing
  this update will ask for.
- **`install.ps1` verifies that same anchor** whenever `ssh-keygen` is present
  (every Windows 10 1809 and later ships it). It falls back to the per-file
  sidecar only for facts about your machine or the release's age — no
  `ssh-keygen`, or a release older than 0.6.0 — and never for something the
  release host chose: a `SHA256SUMS` published without its signature aborts.
- **The installer one-liner is served from the release** rather than a mutable
  branch: `irm https://github.com/isacgalvao/jdk/releases/latest/download/install.ps1 | iex`.
- **Signing left the job that compiles.** The release pipeline is four jobs, and
  the one that runs cargo — and with it every build script in the dependency
  tree — holds a read-only token and no signing identity of any kind. Signing,
  attestation and publishing happen afterwards, in a job that compiles nothing
  and consumes the artifact the build produced.
- cosign is gone and the SLSA build provenance stays: two Sigstore chains from
  the same identity were answering the same question. Verifying a release by
  hand is now `ssh-keygen -Y verify` over `SHA256SUMS` — the same check the
  updater makes, against the same key — plus `gh attestation verify` for the
  zip. Both commands are in the release notes of every release.
- The update bundle is extracted under a ceiling that fits it — 128 MiB and 64
  entries, against four files — instead of the 4 GiB one meant for JDK
  archives.

### Internal

- A tag run's workflow artifact does not contain `SHA256SUMS.sig`: the signature
  is produced by the release job that runs after the build. The GitHub release
  carries both, and that is what `jdk update` and `install.ps1` read.
- Dependabot groups patch and minor updates into one pull request, merged
  automatically once CI has passed on that exact commit, and gives every major
  a pull request of its own. A grouped update once carried six majors of the
  zip library past review.
- `install.ps1`'s verification logic runs in CI against a loopback server and a
  throwaway signing key, covering the paths that decide whether a checksum is
  trusted.

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

[Unreleased]: https://github.com/isacgalvao/jdk/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.6.0
[0.5.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.5.0
[0.4.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.4.0
[0.3.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.3.0
[0.2.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.2.0
[0.1.0]: https://github.com/isacgalvao/jdk/releases/tag/v0.1.0
