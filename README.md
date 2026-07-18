# jdk

A **Windows-first Java version manager**. Real `.exe` shims for `java`, `javac`
and friends resolve the right JDK per project on every invocation, and a
persistent `JAVA_HOME` keeps Maven, Gradle and your IDE pointed at the version
you chose — all **without administrator rights**.

If you have used SDKMAN on Linux or macOS, this is the piece Windows was
missing: clone a project that ships a `.sdkmanrc` and it just works.

## Install

In PowerShell (no admin, no reboot):

```powershell
irm https://raw.githubusercontent.com/isacgalvao/jdk/master/install.ps1 | iex
```

The installer downloads the release for your architecture, verifies its
SHA-256, and runs `jdk setup` once to register `JAVA_HOME`, prepend the store
to your `PATH` and materialize the shims. Then, **in a new terminal** (so it
picks up the new `PATH`):

```powershell
jdk install temurin@21    # download it — the first install becomes your global default
```

That is the whole setup: `java`, `javac` and `JAVA_HOME` work right away. Install
more versions the same way, and switch the global default between them with
`jdk use`:

```powershell
jdk install temurin@17    # a second JDK
jdk use 17                # switch the global default to it
```

To make a single project use a specific version regardless of the global, `cd`
into it and pin:

```powershell
jdk pin temurin@21        # writes .jdkrc here; the shims honor it
```

## Commands

Every selector is either `vendor@version` (`temurin@21`, `zulu@17`) or a bare
version (`21`, `21.0.5`), which uses the default vendor from your config
(`temurin` out of the box).

| Command | What it does |
| --- | --- |
| `jdk install <selector>` | Download and install a JDK |
| `jdk uninstall <selector>` | Remove an installed JDK |
| `jdk use <selector>` | Set the **global** default (retargets the `current` junction) |
| `jdk pin <selector>` | Pin the current directory (writes `.jdkrc`) |
| `jdk list` | List installed JDKs |
| `jdk available [filter] [--latest]` | List JDKs you can install (filter by vendor, version, or both) |
| `jdk current` | Show which Java this directory resolves to, and why |
| `jdk which [tool]` | Print the resolved path of a tool (`java` by default) — handy for IDE setup |
| `jdk setup [--yes]` | One-time Windows prep: `JAVA_HOME`, `PATH`, shims (idempotent) |
| `jdk doctor` | Health-check the store, junction, registry and `PATH`; explain every problem |

## How a version is chosen

On every `java`/`javac` call, the shim walks up from the current directory to
the root of the drive and reads the **first** directory that contains any of
these files, trying them in this order:

```
.jdkrc  →  .sdkmanrc  →  .java-version  →  .tool-versions
```

The first file that names a Java version wins. If no directory up the tree
pins one, the shim falls back to your **global** JDK. This is why cloning a
repository that already has a `.sdkmanrc` (`java=21.0.5-tem`) or an asdf
`.tool-versions` (`java temurin-21`) works with no extra steps — the SDKMAN
vendor suffixes (`tem`, `zulu`, `amzn`, `librca`, `ms`, `graalce`, …) are
understood natively.

> [!IMPORTANT]
> **`jdk use` is not SDKMAN's `use`.** In SDKMAN, `sdk use` changes only the
> current shell for the current session. jdk has no per-session model — the
> shims resolve per project on every invocation — so `jdk use` sets your
> **global** default (it retargets the junction). The per-project knob is
> **`jdk pin`**, which writes a `.jdkrc` the shims pick up through the cascade
> above.

## JAVA_HOME and the junction

`jdk setup` writes `JAVA_HOME` **once**, to `%USERPROFILE%\.jdk\current` — a
directory junction — and that value never changes. `jdk use` moves the
junction to point at a different JDK. Because the path stays the same, every
console and IDE you already have open resolves the new JDK on its next `java`
call: no restart, no logoff. New consoles see the updated `PATH` and
`JAVA_HOME` immediately too, because setup broadcasts a `WM_SETTINGCHANGE`.

## Auto-install

When a project pins a version you don't have installed, the shim can fetch it
on demand. The behavior is set by `auto-install` in your config:

- `prompt` (default) — ask when the terminal is interactive; in CI, fail with
  an actionable message instead of hanging.
- `always` — install without asking.
- `never` — never install from the shim; print what to run.

## Configuration

`%USERPROFILE%\.jdk\config.toml` (a small, flat subset of TOML):

```toml
vendor = "temurin"        # default vendor for bare versions like `21`
auto-install = "prompt"   # always | prompt | never
```

Both keys are optional; the values above are the defaults.

## Environment variables

| Variable | Effect |
| --- | --- |
| `JDK_ROOT` | Store location (default `%USERPROFILE%\.jdk`) |
| `JDK_INDEX` | Override the metadata index base URL |
| `JDK_CAFILE` / `JDK_CAPATH` | Extra CA certificate file / directory for TLS (corporate proxies) |

## Troubleshooting

Run `jdk doctor`. It checks the store layout, the `current` junction, the
registry `JAVA_HOME` and that `PATH` contains the shims and `bin` directories
exactly once, and it names each problem together with how to fix it.

## Roadmap

Planned, but **not** in v0.1:

- a `javaw` GUI shim (windowed Java apps without a console)
- winget and scoop packaging
- Maven `toolchains.xml` integration

## License

Apache-2.0. See [LICENSE](LICENSE).
