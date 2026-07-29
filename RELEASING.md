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

On the `v*` tag, `release.yml` runs five jobs — four in a line, and one
alongside the last:

- **gates**: `ci.yml` at the tagged commit — the set in §4.
- **build** (`windows-2025`, read-only token): resolves the version and
  re-checks the tag == Cargo version and the CHANGELOG section; `cargo publish
  --dry-run` for the three crates; builds `jdk.exe` (SBOM embedded via `cargo
  auditable`) and `jdk-shim.exe` (size-gated < 1 MiB), x64 and best-effort
  arm64; assembles per-arch zips with `LICENSE` + `README.md`, renders the
  scoop manifest `jdk.json` from the hashes it just computed, writes per-file
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
- **winget** (`needs: [build, release]`, tag only, `permissions: {}`,
  `continue-on-error`): downloads a pinned `komac`, checks its SHA-256 and
  opens the winget-pkgs pull request for the new version. It runs beside
  `publish-crates`, not after it — a winget-pkgs outage has no business
  holding up crates.io — and it cannot fail the release, which is already
  published and signed by the time it starts. **Skips itself when the
  `WINGET_TOKEN` secret is absent**, which is where the repository sits until
  the package has been submitted once by hand (below). It also refuses to
  submit a release that is missing an architecture; see below.

A `workflow_dispatch` run stops after **build**: gates, build, package, scan and
artifact, with no signature, no release and no publish.

The `release` **environment** is a repository setting (Settings → Environments),
not something the workflow file can create. It has to exist and hold
`CARGO_REGISTRY_TOKEN`, or `publish-crates` cannot run; it is also where a
required reviewer or a wait timer goes, if the maintainer wants the crates.io
upload to stop for a human.

## 7. Verify the published release

1. The GitHub release exists with the zips, `.sha256` sidecars, `SHA256SUMS`,
   `SHA256SUMS.sig`, `install.ps1` and `jdk.json` attached. **`SHA256SUMS.sig`
   is not optional** — without it every `jdk update` from 0.6.0 on refuses this
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
6. The **winget** job opened a pull request against `microsoft/winget-pkgs`, or
   said in a notice why it did not. Merging is theirs, not ours, and can take
   days; nothing else waits on it.

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

## The package managers

Two channels beside the one-liner, and both need the same thing said out loud:
**neither can run `jdk setup`, and neither can undo it.** winget has no
post-install hook at all, and scoop's `pre_uninstall` fires on `scoop update`
as well as on uninstall (`libexec/scoop-update.ps1:341`), so wiring `jdk setup
--undo` into it would tear a user's machine down on every upgrade.

Pairing that with a `post_install` that re-runs `jdk setup --yes` does undo the
undo, and was rejected on three counts: it re-does a setup the user may have
deliberately reversed, it replaces a foreign `JAVA_HOME` unasked on every
upgrade, and an update that fails between the two hooks leaves the machine with
no `JAVA_HOME`, no `PATH` entries and no shims. The scoop wiki documents a
`$cmd` variable that would let a hook tell an update from an uninstall and make
this safe — it is not real: `Invoke-HookScript` (`lib/install.ps1:147`) passes
only the hook type, the manifest and the architecture, and `$cmd` appears
nowhere in `lib/install.ps1`, `libexec/scoop-update.ps1` or
`libexec/scoop-uninstall.ps1`. Do not build on it without testing it first.

The README says as much on the install page; do not quietly promise otherwise.

There is a second consequence, worth knowing before reading a bug report about
it: `jdk setup` copies the running `jdk.exe` into `<store>\bin` and
**prepends** that directory to the user `PATH`, ahead of winget's `Links` and
scoop's `shims`. After setup the store copy is the one that runs and `jdk
update` is what maintains it; `winget upgrade` and `scoop update` refresh a
copy that no longer answers.

### When a release loses arm64

The aarch64 build is `continue-on-error`, so a release can ship without its
arm64 zip. The three channels answer that differently, on purpose:

- **The release itself still goes out.** If the previous release had an arm64
  zip and this one does not, `build` raises a `::warning::` naming the shrink
  and stops there. It is not a throw, because making a missing aarch64 build
  fatal would settle **UX-12** — whether arm64 is a supported architecture or a
  best-effort extra — as a side effect of a packaging change. Promoting arm64
  turns that warning into a throw, and this is the line to change.
- **scoop gets an x64-only manifest.** An arm64 user is then told the app does
  not support their architecture. On Windows 11 that costs nothing worth
  naming: an arm64 machine runs the x64 build under emulation. On Windows 10
  on ARM64 there is no such fallback, and `scoop update` removes the installed
  version before it validates the new one, with no rollback — so a user there
  can be left with neither.
- **winget gets nothing.** The `winget` job throws rather than submit. komac
  maps the URLs it is given onto the architectures the manifest already in
  winget-pkgs declares, and one URL for a two-architecture package makes it
  either refuse the duplicate entry or write an arm64 installer pointing at
  the x64 zip — a duplicate installer hash is only a warning there, so that
  one publishes. A version absent from winget-pkgs can be submitted later; a
  manifest naming the wrong bytes stays until someone notices.

### scoop — nothing to keep in sync

`packaging/scoop/jdk.json` is a template. The build job renders it with the
hashes it has just computed and ships it as a release asset, covered by
`SHA256SUMS` like everything else; users install from
`releases/latest/download/jdk.json`, and scoop records that URL and re-reads it
on `scoop update`. No bucket, no second repository, no manifest to bump.

Edit the template when the description, the notes or the `autoupdate` block
should change. `render.ps1` beside it shows what a release would publish:

```powershell
./packaging/scoop/render.ps1 -Version 0.6.0 -OutFile jdk.json `
  -X64Sha256 <hash> -Arm64Sha256 <hash>
```

### winget — one manual submission, then automatic

`komac update` needs a version already in winget-pkgs to copy the metadata
from, so the first one goes by hand, once:

1. Fork `microsoft/winget-pkgs` to the account that owns this repository. That
   fork is where komac pushes its branch, by hand and from CI alike.
2. Render the manifest for a **published** release and validate it:

   ```powershell
   ./packaging/winget/render.ps1 -Tag v0.6.0 `
     -OutDir <winget-pkgs>\manifests\i\isacgalvao\jdk\0.6.0
   ```

   It takes the hashes from that release's **signed** `SHA256SUMS` and runs
   `winget validate` on what it wrote.
3. Open the pull request from the fork — one version per pull request.
4. Once it is merged, create the repository secret **`WINGET_TOKEN`**: a
   **classic** personal access token scoped `public_repo`. A fine-grained token
   cannot open a pull request against a repository the account neither owns nor
   belongs to. From the next tag on, the `winget` job does the rest.
5. Take the pending wording out of the README — **both halves of it**. The
   "Built and waiting on someone else" paragraph in the Roadmap goes, and the
   Install section's winget entry loses its condition, becoming just the
   command. Each exists to keep the README true while the package is pending
   and is false the moment it is not; leaving only one behind points a reader
   at an anchor that no longer says anything, and talks them out of a command
   that works.

In that order. Until the secret exists the job skips with a notice, and until
the pull request is merged `winget install isacgalvao.jdk` finds no package —
which is why the README states it as a condition rather than a fact, and why
the Roadmap carries the pending line until step 5.

`WINGET_TOKEN` is the widest credential this repository holds: `public_repo`
grants write to every public repository the account owns, this one included.
That is why the `winget` job pins `komac` by version **and** SHA-256 and uses
no action at all, rather than `winget-releaser` — whose own
`cargo-binstall@main` step would run unpinned code in the job that holds it.

`packaging/winget/*.yaml` stay on as the source of the metadata komac carries
forward: the description, the tags, the moniker. Editing them changes nothing
by itself — a metadata change reaches winget-pkgs through a `komac update` run
by hand, or a pull request of its own.

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
