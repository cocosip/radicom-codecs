# Repository Guidelines

## Project Structure

This repository is a Cargo workspace for pure-Rust DICOM image codecs. The public
facade and registry live in `crates/radicom-codecs`; codec families are split into
`radicom-codecs-rle`, `radicom-codecs-jpeg`, `radicom-codecs-jpeg-ls`, and
`radicom-codecs-jpeg2000`. The internal `radicom-codecs-tool` crate provides the
`radicom-compress` CLI. Unit tests sit beside implementation code, integration
tests are under each crate's `tests/` directory, examples are under `examples/`,
and design/development notes are in `docs/`. Keep native interop harnesses and
generated results in the ignored `.interop/` directory.

## Build, Test, and Development Commands

Run the same checks used by CI before submitting changes:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

Use `cargo test -p <crate>` for a focused iteration, or install the local CLI
with `cargo install --path crates/radicom-codecs-tool`. The supported MSRV is
Rust 1.89; verify compatibility with `cargo check --workspace --all-features`
under that toolchain when changing language or dependency requirements.

## Coding Style & Naming

Use Rust 2021 conventions and let `rustfmt` determine formatting. Types and
traits use `UpperCamelCase`, functions, modules, and tests use `snake_case`, and
constants use `SCREAMING_SNAKE_CASE`. Prefer existing codec traits, error types,
and registry patterns over new wrappers. Preserve validation and managed errors
at DICOM input boundaries; do not add native codec dependencies or FFI.

## Testing Guidelines

Add focused unit tests for codec algorithms and malformed-input paths, plus
facade/transcoder integration coverage when registration or public behavior
changes. Name tests after the behavior they prove. Run the workspace test command
above; run `.interop` scripts separately when changing wire compatibility because
they depend on external reference implementations.

## Commits & Pull Requests

Use Gitmoji Conventional Commits, for example `🐛 fix(jpeg): reject malformed SOS`
or `✨ feat(tool): add format listing`. Keep commits focused. Pull requests should
describe the behavior change, affected crates, validation commands, and any
interop evidence or limitations. Update relevant design or README documentation,
and call out compatibility or release-version changes explicitly.
