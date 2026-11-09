# Releasing radicom-codecs

Publishing is **tag-driven**. Pushing a `v<version>` tag is the only trigger;
`master`/`main` builds and tests but never publishes (see `.github/workflows/ci.yml`
and `.github/workflows/release.yml`).

The workspace carries **one version** in `[workspace.package]` (`Cargo.toml`) and
repeats it as literal pins under `[workspace.dependencies]`. The five published
crates are:

| Order | Crate | Notes |
| --- | --- | --- |
| 1 | `radicom-codecs-rle` | depends only on `radicom` |
| 2 | `radicom-codecs-jpeg` | depends only on `radicom` |
| 3 | `radicom-codecs-jpeg-ls` | depends only on `radicom` |
| 4 | `radicom-codecs-jpeg2000` | depends only on `radicom` |
| 5 | `radicom-codecs` | facade; depends on the four families |

`radicom-codecs-tool` (the `radicom-compress` CLI) is an internal debugging
utility and is `publish = false`: it is installed from the repository
(`cargo install --path crates/radicom-codecs-tool`) and is never a crates.io
release artifact.

The published `0.1.2` release uses `radicom 0.1`. This checkout upgrades the core
dependency to `radicom 0.2.0`, while the workspace package version still reads
`0.1.2`. Before publishing this change, bump all workspace versions and internal
pins to a new release (use `0.2.0` for the incompatible core-type change).
Existing `0.1.2` artifacts on crates.io cannot be overwritten, and the publish
job skips versions that already exist.

## Prerequisites

- Push access to `cocosip/radicom-codecs`.
- Repository secret **`CARGO_REGISTRY_TOKEN`**: a crates.io API token allowed to
  publish the five library crates.
- The `radicom` core version the workspace depends on is already on crates.io
  (`cargo search radicom`). The release build resolves it from the registry, not
  from a local checkout.

## Bumping the version

A release bump edits the literals in the root `Cargo.toml` together — leaving any
one stale makes `cargo build` fail before publish:

1. `[workspace.package] version = "X.Y.Z"`.
2. The five `version = "X.Y.Z"` literals under `[workspace.dependencies]`
   (`radicom-codecs*`). Keep `radicom = "..."` at the published core version.
3. Refresh the tracked lockfile: `cargo build --workspace --all-features` (or
   `cargo update -p radicom-codecs --precise X.Y.Z` per crate), then commit
   `Cargo.lock` with the bump.
4. Update installation examples and compatibility notes in `README.md` and the
   design/development docs. Consumers must use the same core version as the
   codecs because codec types and the global registry belong to that version.

## Local pre-flight

Run the same gates the tag workflow re-runs:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features
$env:RUSTDOCFLAGS = '-D warnings'
cargo doc --workspace --all-features --no-deps
cargo +1.89.0 check --workspace --all-features
# Every published crate must ship LICENSE and README.md with valid metadata.
# `--list` also works before new internal dependency versions are published.
foreach ($pkg in 'radicom-codecs-rle','radicom-codecs-jpeg','radicom-codecs-jpeg-ls',
                 'radicom-codecs-jpeg2000','radicom-codecs') {
    cargo package -p $pkg --list
}
```

## Cutting the release

```powershell
# After updating Cargo.toml and Cargo.lock to the chosen new version:
$releaseVersion = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).packages[0].version
git tag "v$releaseVersion"
git push origin "v$releaseVersion"
```

What `.github/workflows/release.yml` then does:

1. **verify-tag** — the tag must equal every publishable crate's version and the
   publishable set must be exactly the five crates above, so a newly added crate
   can never be left unpublished.
2. **test / msrv** — re-runs fmt, clippy (default + all features), tests (default
   + all features), docs, the packaged-files check, and the MSRV build.
3. **publish** — publishes the five crates in dependency order. It is idempotent:
   any version already on crates.io is skipped, and a fresh crate is retried for
   index propagation, so a partially failed run is fixed by re-running the same
   tag.
4. **Release notes** (in the `publish` job) — creates a GitHub Release whose notes
   compare the previous tag with this one (or mark "initial release" when there
   is no earlier tag).

The publish and release-note work run in one job; the workflows mirror the core
`radicom` repository. To **regenerate the release notes** for an already-published
tag without republishing, run the Release workflow manually and pass the `tag`
input — publishing is skipped and only the notes/release are rewritten.

## Recovery

- **A step failed mid-publish:** re-run the failed workflow/job for the same tag.
  Already-published crates are skipped; only the remaining ones are pushed.
- **A bad version went out:** `cargo yank --version X.Y.Z <crate>` (repeat for each
  affected crate), then release a fixed `X.Y.(Z+1)`. Yanking is reversible with
  `cargo yank --undo`.

Only the five library crates are published. `radicom-codecs-tool` is
`publish = false` and therefore never enters `cargo publish`, the `verify-tag`
expected set, or the release loop.
