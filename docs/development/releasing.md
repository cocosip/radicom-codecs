# Releasing radicom-codecs

Publishing is **tag-driven**. Pushing a `v<version>` tag is the only trigger;
`master`/`main` builds and tests but never publishes (see `.github/workflows/ci.yml`
and `.github/workflows/release.yml`).

The workspace carries **one version** in `[workspace.package]` (`Cargo.toml`) and
repeats it as literal pins under `[workspace.dependencies]`. The six published
crates are:

| Order | Crate | Notes |
| --- | --- | --- |
| 1 | `radicom-codecs-rle` | depends only on `radicom` |
| 2 | `radicom-codecs-jpeg` | depends only on `radicom` |
| 3 | `radicom-codecs-jpeg-ls` | depends only on `radicom` |
| 4 | `radicom-codecs-jpeg2000` | depends only on `radicom` |
| 5 | `radicom-codecs` | facade; depends on the four families |
| 6 | `radicom-codecs-tool` | ships the `radicom-compress` binary; depends on the facade |

The first release is **`0.1.0`**, matching the published `radicom = "0.1.0"` core
crate it depends on. `radicom-codecs` is not yet on crates.io, so `0.1.0` is the
initial version.

## Prerequisites

- Push access to `cocosip/radicom-codecs`.
- Repository secret **`CARGO_REGISTRY_TOKEN`**: a crates.io API token allowed to
  publish. On the very first release the six crate *names* do not exist yet; the
  token owner creates them on first publish.
- The `radicom` core version the workspace depends on is already on crates.io
  (`cargo search radicom`). The release build resolves it from the registry, not
  from a local checkout.

## Bumping the version (future releases)

A release bump edits the literals in the root `Cargo.toml` together — leaving any
one stale makes `cargo build` fail before publish:

1. `[workspace.package] version = "X.Y.Z"`.
2. The five `version = "X.Y.Z"` literals under `[workspace.dependencies]`
   (`radicom-codecs*`). Keep `radicom = "..."` at the published core version.
3. Refresh the tracked lockfile: `cargo build --workspace --all-features` (or
   `cargo update -p radicom-codecs --precise X.Y.Z` per crate), then commit
   `Cargo.lock` with the bump.
4. Docs reference the version in a few places (`README.md`, `docs/design/*`); a
   grep for the old version catches stragglers.

## Local pre-flight

Run the same gates the tag workflow re-runs:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
# Every published crate must ship LICENSE and README.md with valid metadata.
# `--list` is used because before the first release a crate's own path deps are
# not on crates.io yet, so a full `cargo package` cannot resolve them.
foreach ($pkg in 'radicom-codecs-rle','radicom-codecs-jpeg','radicom-codecs-jpeg-ls',
                 'radicom-codecs-jpeg2000','radicom-codecs','radicom-codecs-tool') {
    cargo package -p $pkg --list
}
```

## Cutting the release

```powershell
git tag v0.1.0
git push origin v0.1.0
```

What `.github/workflows/release.yml` then does:

1. **verify-tag** — the tag must equal every publishable crate's version and the
   publishable set must be exactly the six crates above, so a newly added crate
   can never be left unpublished.
2. **test / msrv** — re-runs fmt, clippy (default + all features), tests (default
   + all features), docs, the packaged-files check, and the MSRV build.
3. **publish** — publishes the six crates in dependency order. It is idempotent:
   any version already on crates.io is skipped, and a fresh crate is retried for
   index propagation, so a partially failed run is fixed by re-running the same
   tag. Publishing is serialized (`concurrency: release`, no cancel).
4. **github-release** — creates a GitHub Release whose notes compare the previous
   tag with this one (or mark "initial release" when there is no earlier tag).

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

`radicom-codecs-tool` publishes the `radicom-compress` binary but is not required
for library consumers; if only the libraries are needed, the tool crate can be
omitted from the ordering in `release.yml` (update both the loop and the
`verify-tag` expected set together).
