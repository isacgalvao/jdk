# Releasing jdk

The release checklist for `jdk`. The workflow does the signing and publishing;
this file defines the order and the gates a maintainer runs to get there.

`jdk` ships a **single version** for the whole workspace: bump it, document it,
tag it, push. A tag push does everything else automatically.

## Release drivers

- `.github/workflows/release.yml` is the canonical pipeline. A **tag push**
  (`v*`) is the only thing that signs artifacts, publishes the GitHub release and
  pushes the crates to crates.io.
- `workflow_dispatch` on the same workflow is a **dry-run**: it builds, scans and
  uploads the same artifacts as *workflow artifacts* (unsigned, no GitHub
  release, no crates.io). Use it to exercise the pipeline before tagging.
- Do **not** cut a pre-release (RC) tag. Any `v*` tag — `v0.3.0-rc.1` included —
  triggers the publish job, and the version check aborts because the tag
  (`0.3.0-rc.1`) will not match the workspace version (`0.3.0`). The dry-run
  above is the pre-flight, not an RC tag.

## 1. Pre-flight (optional but recommended)

If the release workflow, signing, packaging or the shim changed since the last
release, run a dry-run first and confirm the artifacts look right:

- Actions → **release** → *Run workflow* on `master`.
- It builds x64 (+ best-effort arm64), runs the Defender scan, and uploads the
  `dist/` zips + `SHA256SUMS` as a workflow artifact. Download and sanity-check.

## 2. Version coherence

`jdk` is a single-version workspace. `scripts/check-versions.ps1` enforces the
invariant, but bump it deliberately, not by trial and error.

1. Bump `[workspace.package] version` in the root `Cargo.toml`.
2. Bump the **three internal pins** that must echo that version:
   - `crates/jdk-core/Cargo.toml` → `jdk-resolve`
   - `crates/jdk/Cargo.toml` → `jdk-core`
   - `crates/jdk/Cargo.toml` → `jdk-resolve`

   (Unversioned path deps — `test-support`, the `jdk-index-gen` deps — are not
   pinned and need no change.)
3. Regenerate the lockfile: `cargo check` (the `Cargo.lock` is committed).
4. If the MSRV moved, update `rust-version` **and** the README `MSRV-x.y` badge —
   they must match.

**Semver:** a new vendor/feature that stays backward-compatible is a **minor**
bump (that is what `0.1.0 → 0.2.0` was); a breaking change to the CLI or config
is a major once past 1.0.

## 3. Changelog

`CHANGELOG.md` is **hand-written**, in [Keep a Changelog] order. There is no
generator — the prose is curated.

1. Move the accumulated bullets from `## [Unreleased]` into a new dated section:

   ```
   ## [X.Y.Z] - YYYY-MM-DD
   ```

2. Group under the standard headings (`### Added`, `### Changed`, `### Fixed`,
   `### Removed`, `### Security`) — only the ones that apply.
3. Write for a user reading release notes, not a commit log: what changed and why
   it matters, one bullet per user-visible change.
4. Leave an empty `## [Unreleased]` at the top for the next cycle.

This section is the release notes **verbatim** — `release.yml` copies it into the
GitHub release, so it must read well on its own.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/

## 4. Local gates

Run these from the repo root before tagging. They mirror CI (`check` + `deny`
jobs) plus the release build's own audit:

```powershell
./scripts/check-versions.ps1          # pins, MSRV badge, CHANGELOG section, publish set
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check                      # license / advisory allowlist
cargo audit                           # known-vulnerable deps (release build runs this too)
```

`check-versions.ps1` fails if any internal pin drifts from the workspace version,
if the README MSRV badge drifts from `rust-version`, if `CHANGELOG.md` has no
`## [X.Y.Z]` section for the new version, or if a crate outside the published set
(`jdk`, `jdk-core`, `jdk-resolve`) became publishable. Green here means the tag
will not be rejected by the workflow.

## 5. Cut the release

1. Commit the bump and changelog together:

   ```powershell
   git add Cargo.toml Cargo.lock crates CHANGELOG.md README.md
   git commit -m "chore(release): vX.Y.Z"
   ```

2. Create an **annotated** tag whose name matches the workspace version exactly
   (the workflow aborts if `vX.Y.Z` ≠ `Cargo.toml` version):

   ```powershell
   git tag -a vX.Y.Z -m "jdk vX.Y.Z"
   ```

3. Push `master`, then the tag:

   ```powershell
   git push origin master
   git push origin vX.Y.Z
   ```

The tag push is the point of no return — everything after is automatic.

## 6. What the workflow does (automatic)

On the `v*` tag, `release.yml` runs two jobs:

- **release** (`windows-2025`): resolves the version and re-checks the tag ==
  Cargo version and the CHANGELOG section; `cargo audit`; builds `jdk.exe`
  (SBOM embedded via `cargo auditable`) and `jdk-shim.exe` (size-gated < 1 MiB),
  x64 and best-effort arm64; assembles per-arch zips with `LICENSE` + `README.md`,
  per-file `.sha256` sidecars and an aggregate `SHA256SUMS`; scans `dist/` with
  Defender; **signs `SHA256SUMS` keyless with cosign** (`.sigstore.json` bundle);
  attaches **SLSA build-provenance attestations** to the zips; and publishes the
  GitHub release with the CHANGELOG section as notes plus install/verify blocks.
- **publish-crates** (`needs: release`): `cargo publish` in dependency order
  **jdk-resolve → jdk-core → jdk**, waiting for each to index. Uses the
  `CARGO_REGISTRY_TOKEN` secret. `jdk-index-gen` and `test-support` are
  `publish = false`; `jdk-shim` is not a published crate.

## 7. Verify the published release

1. The GitHub release exists with the zips, `.sha256` sidecars, `SHA256SUMS`,
   `SHA256SUMS.sigstore.json` and `install.ps1` attached.
2. Signature and provenance verify:

   ```bash
   cosign verify-blob SHA256SUMS --bundle SHA256SUMS.sigstore.json \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com \
     --certificate-identity-regexp '^https://github.com/isacgalvao/jdk/.github/workflows/release.yml@'
   gh attestation verify jdk-vX.Y.Z-windows-x64.zip --repo isacgalvao/jdk
   ```

3. crates.io shows the new version for `jdk-resolve`, `jdk-core` and `jdk`.
4. A clean install works:

   ```powershell
   irm https://raw.githubusercontent.com/isacgalvao/jdk/master/install.ps1 | iex
   jdk --version   # must print X.Y.Z
   ```

## 8. If something fails

- **Tag rejected (version mismatch / missing CHANGELOG section):** the tag is
  already pushed but nothing published. Fix the source, delete and recreate the
  tag on the corrected commit:

  ```powershell
  git push origin :refs/tags/vX.Y.Z   # delete remote tag
  git tag -d vX.Y.Z                    # delete local tag
  # fix, commit, re-tag, re-push
  ```

- **crates.io publish failed after the GitHub release succeeded:** do **not**
  re-run the whole workflow (it would try to recreate the release). Publish the
  remaining crates manually, in order, skipping any already at the target
  version:

  ```powershell
  cargo publish -p jdk-resolve
  cargo publish -p jdk-core
  cargo publish -p jdk
  ```

## Do not do

- Do not push a tag whose name differs from the workspace version — the workflow
  aborts on the mismatch.
- Do not use an RC (`-rc.N`) tag; use the `workflow_dispatch` dry-run instead.
- Do not re-run the release workflow for a tag that already published — recover
  the specific failed step manually.
- Do not hand-edit the GitHub release notes to diverge from the CHANGELOG; edit
  `CHANGELOG.md` and, if needed, re-cut.
