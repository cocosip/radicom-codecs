# radicom-codecs

Concrete DICOM image codec implementations for the [radicom](https://crates.io/crates/radicom)
core library. The codec contract, parameter types, pixel-data facade and
transcoder live in `radicom`; this workspace implements that contract and
registers the concrete codecs, mirroring the relationship between
`FO-DICOM.Core` and `fo-dicom.Codecs`.

## Crates

| Crate | Contents |
|---|---|
| **`radicom-codecs`** | Public facade: registry, registration and re-exports |
| **`radicom-codecs-rle`** | RLE Lossless |
| **`radicom-codecs-jpeg`** | Classic JPEG: Process 1, Process 2/4, Lossless 14, Lossless 14 SV1 |
| **`radicom-codecs-jpeg-ls`** | JPEG-LS Lossless and Near-Lossless |
| **`radicom-codecs-jpeg2000`** | Classic JPEG 2000 and HTJ2K |
| **`radicom-codecs-tool`** | `radicom-compress` CLI |

## Usage

```rust
radicom_codecs::install();
```

`install()` registers all twelve supported transfer syntaxes in the
process-wide `radicom` codec registry, so `radicom`'s pixel-data facade and
transcoder can encode and decode them. Each family crate can also be used
directly for its concrete codec types.

## Development

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI runs on pushes and pull requests to `master`/`main`. Pushing a `v<version>`
tag runs the release workflow, which verifies the tag against the single
workspace version, re-runs every CI gate, publishes the crates to crates.io and
creates a GitHub release whose notes compare the previous and current version.

## License

MIT
