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

`jdk` is a single-version workspace: every crate inherits `version.workspace`,
and the internal dependencies resolve through `[workspace.dependencies]`, so the
version lives in exactly one file — the root `Cargo.toml`, in two spots. The
crate manifests are not touched at all: they carry `version.workspace = true`
and `jdk-core.workspace = true`, with no version of their own to bump.

1. Bump `[workspace.package] version` in the root `Cargo.toml`.
2. Bump the **same version** in the `[workspace.dependencies]` requirements just
   below it — `jdk-core` and `jdk-resolve` each carry a `version = "X.Y.Z"`
   beside their `path`. Cargo has no way to inherit the package version into a
   dependency requirement, which is why it is written twice.

   Forgetting the second is **not** a silent drift: the path dependency stops
   satisfying the requirement, so `cargo check` fails to resolve and says so.
   That is a stronger gate than a version-checking script, which is why there is
   no longer one. `test-support` is never published and carries no requirement,
   so it needs no change.
3. Regenerate the lockfile: `cargo check` (the `Cargo.lock` is committed).
4. If the MSRV moved, update `rust-version` **and** the README `MSRV-x.y` badge —
   they must match. CI's `msrv` job reads `rust-version` straight from the
   manifest and runs `cargo check` on that exact toolchain, so a claim the code
   no longer supports fails there. The badge is prose in a `<img>` URL that no
   job reads: it is the half only a human catches.

**Versioning:** `0.MINOR.PATCH`, and the major stays at zero. **MINOR** covers
anything observable — a new feature, a removed or renamed command, flag or exit
code, a change to the config or pin format. **PATCH** is a fix that does not
change expected behaviour, plus docs, performance and dependencies. In 0.x the
minor slot already carries incompatible change, so nothing is lost by never
reaching 1.0, and stabilizing the contract stays a deliberate decision rather
than a side effect of a version number.

Two contracts version separately from the product. The **index schema** carries
its own `version` in `index.json` and moves when the wire format does — an
additive change does not bump it, a change of shape does — and since 0.6.0 that
number is load-bearing: a client meeting an index above the version it reads
refuses the file and names the way to one that reads it, instead of guessing at
a shape nobody told it about. The rule is written out in `jdk-core/src/index.rs`
beside the constant. The **Rust API** of `jdk-core` and `jdk-resolve` carries no
contract at all: those crates are on crates.io only so `cargo install jdk` can
resolve them, and both say so in their own docs — their version tracks the CLI,
not their API.

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

## 4. Local gates (optional)

`release.yml` runs `ci.yml` at the tagged commit before it builds anything, so a
tag pushed on a commit that never went through CI still cannot reach crates.io.
These are for meeting a failure before the tag rather than after it:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny --locked check                 # license / advisory allowlist
./.github/scripts/test_install_anchor.ps1 # install.ps1's checksum anchor
```

Two checks have no local form: that the tag matches `[workspace.package]
version`, and that `CHANGELOG.md` carries a `## [X.Y.Z]` section for it. Both
run before the first artifact is built.

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

On the `v*` tag, `release.yml` runs four jobs in a line:

- **gates**: `ci.yml` at the tagged commit — the set in §4.
- **build** (`windows-2025`, read-only token): resolves the version and
  re-checks the tag == Cargo version and the CHANGELOG section; `cargo publish
  --dry-run` for the three crates; builds `jdk.exe` (SBOM embedded via `cargo
  auditable`) and `jdk-shim.exe` (size-gated < 1 MiB), x64 and best-effort
  arm64; assembles per-arch zips with `LICENSE` + `README.md`, per-file
  `.sha256` sidecars and an aggregate `SHA256SUMS`; scans `dist/` with Defender;
  uploads `dist/` as a workflow artifact.
- **release** (`needs: build`, tag only): downloads that artifact, **signs
  `SHA256SUMS` with `ssh-keygen -Y sign`** (`SHA256SUMS.sig`, namespace
  `jdk-release`, key from the `RELEASE_SIGNING_KEY` secret), attaches **SLSA
  build-provenance attestations** to the zips, and publishes the GitHub release
  with the CHANGELOG section as notes plus install/verify blocks. It is the only
  job holding write access or an OIDC token, and it compiles nothing: no build
  script in the dependency tree ever shares an environment with either.
- **publish-crates** (`needs: release`, `environment: release`): `cargo publish`
  in dependency order **jdk-resolve → jdk-core → jdk**, waiting for each to
  index. Uses the `CARGO_REGISTRY_TOKEN` secret. `jdk-index-gen` and
  `test-support` are `publish = false`; `jdk-shim` is not a published crate.

A `workflow_dispatch` run stops after **build**: gates, build, package, scan and
artifact, with no signature, no release and no publish.

The `release` **environment** is a repository setting (Settings → Environments),
not something the workflow file can create. It has to exist and hold
`CARGO_REGISTRY_TOKEN`, or `publish-crates` cannot run; it is also where a
required reviewer or a wait timer goes, if the maintainer wants the crates.io
upload to stop for a human.

## 7. Verify the published release

1. The GitHub release exists with the zips, `.sha256` sidecars, `SHA256SUMS`,
   `SHA256SUMS.sig` and `install.ps1` attached. **`SHA256SUMS.sig` is not
   optional** — without it every `jdk update` from 0.6.0 on refuses this
   release.
2. Signature and provenance verify:

   ```bash
   echo 'release@jdk namespaces="jdk-release" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKufZs1YJGeiBZsIVZSpxMIR/1hmAu1/biqqKJzgDxu7' > allowed_signers
   ssh-keygen -Y verify -f allowed_signers -I release@jdk -n jdk-release \
     -s SHA256SUMS.sig < SHA256SUMS
   gh attestation verify jdk-vX.Y.Z-windows-x64.zip --repo isacgalvao/jdk
   ```

3. crates.io shows the new version for `jdk-resolve`, `jdk-core` and `jdk`.
4. A clean install works:

   ```powershell
   irm https://github.com/isacgalvao/jdk/releases/latest/download/install.ps1 | iex
   jdk --version   # must print X.Y.Z
   ```

5. `jdk update` accepts the release. The check above proves the signature is
   good; this proves the *updater* agrees, which is the part that can break on
   its own — a key rotated in one place and not the other verifies by hand and
   still bricks every self-update.

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

## The release signing key

`SHA256SUMS` is signed with an ed25519 key held in the `RELEASE_SIGNING_KEY`
repository secret (an OpenSSH private key, no passphrase — the workflow runs
unattended). The matching public key is **pinned in five places**, and they
must always agree:

| Where | What it is |
|---|---|
| `crates/jdk-core/src/release.rs` (`RELEASE_PUBKEY`) | what `jdk update` trusts |
| `install.ps1` (`$signingKey`) | what a fresh install trusts |
| `README.md` (twice: the displayed key and the `allowed_signers` snippet) | what a user verifying by hand trusts |
| `RELEASING.md` § 7 | what the maintainer verifies with |
| `.github/workflows/release.yml` (release-notes "Verify" block) | what every release page tells users to trust |

The workflow materializes the secret into `$RUNNER_TEMP` (normalizing line
endings — an editor that saved the key with CRLF would otherwise produce a file
OpenSSH rejects), hardens its ACL, signs, and deletes it in a `finally` that
then asserts the file is gone. The key never reaches the repository or an
artifact.

### Rotation

A rotation is a **minor release**, never a patch: an installed `jdk` only ever
trusts the key it shipped with, so the new key has to travel inside a release
signed by the *old* one. Skipping that strands every existing installation.

1. Generate the pair, off the runner:
   `ssh-keygen -t ed25519 -N "" -C "jdk-release-<year>" -f jdk-release`
2. Replace the `RELEASE_SIGNING_KEY` secret with the contents of
   `jdk-release` (the private half). Keep the old secret value until step 5
   succeeds.
3. Update the public key in all five places in the table above.
4. Release as a **minor** bump. This release is still signed with the OLD
   key — it is what carries the new key to installed clients.
5. Verify that a client from **before** step 4 can `jdk update` onto it, and
   that a client from **after** can update onto the next release.
6. Only then destroy the old private key.

Never rotate and change the signing format in the same release: if an update
fails, one of the two is at fault and there is no way to tell which from the
error the user reports.

### If the key is compromised

Assume every release signed with it is suspect, including ones that look
untouched.

1. Rotate immediately, following the steps above — the compromised key still
   signs the release that carries the replacement, because there is no other
   way to reach installed clients. Publish it fast rather than perfectly.
2. Delete or re-sign the affected releases. A release whose `SHA256SUMS.sig`
   is removed is refused by every 0.6.0+ updater, which is the correct failure:
   loud, not silent.
3. Say so in the release notes of the rotating release and in the CHANGELOG,
   naming the affected versions. Users who installed by hand have no other
   signal.
4. Revoke the runner's access path if that is how it leaked (see the
   `RELEASE_SIGNING_KEY` secret's audit log) before re-enabling releases.

## What the updater expects from a release

`jdk update` reads a release through a fixed contract. Breaking any line of it
breaks self-update for every installed client, and the client cannot be fixed
after the fact — it is already out there.

- **`/releases/latest` redirects** to `/releases/tag/v<version>`. The version
  is read from that redirect target; GitHub does this automatically for a
  non-draft, non-prerelease release.
- **A release marked pre-release does not exist** as far as `latest` is
  concerned — the redirect skips it. This is a second reason not to cut RC
  tags, on top of the version check that already aborts them.
- **Assets are named `jdk-v<version>-windows-<arch>.zip`**, `<arch>` being
  `x64` or `arm64`. The name is what the signed `SHA256SUMS` line is looked up
  by, so it is load-bearing, not cosmetic: it is what ties the announced
  version to the bytes.
- **`SHA256SUMS` and `SHA256SUMS.sig` are both mandatory**, at
  `/releases/download/v<version>/`. A release missing either is refused, on
  purpose — "unsigned" must never be a state a release host can put a client
  into.
- **`SHA256SUMS` covers every published asset**, one `<hash>  <name>` line
  each. A zip with no line is refused even though it downloaded fine.

The per-file `.sha256` sidecars are **not** part of this contract. `jdk update`
ignores them; they exist for `install.ps1`'s fallback and for humans.

## Do not do

- Do not push a tag whose name differs from the workspace version — the workflow
  aborts on the mismatch.
- Do not use an RC (`-rc.N`) tag; use the `workflow_dispatch` dry-run instead.
- Do not re-run the release workflow for a tag that already published — recover
  the specific failed step manually.
- Do not hand-edit the GitHub release notes to diverge from the CHANGELOG; edit
  `CHANGELOG.md` and, if needed, re-cut.
- Do not delete or replace `SHA256SUMS.sig` on a published release, and do not
  edit `SHA256SUMS` after the fact — the signature stops matching and every
  installed client refuses that release.
