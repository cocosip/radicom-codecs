# radicom-codecs

[![Crates.io](https://img.shields.io/crates/v/radicom-codecs.svg)](https://crates.io/crates/radicom-codecs)
[![Docs.rs](https://docs.rs/radicom-codecs/badge.svg)](https://docs.rs/radicom-codecs)
[![CI](https://github.com/cocosip/radicom-codecs/actions/workflows/ci.yml/badge.svg)](https://github.com/cocosip/radicom-codecs/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Concrete DICOM image codecs for the [`radicom`](https://crates.io/crates/radicom)
core library. `radicom` owns the codec contract, parameter types, pixel-data facade
and transcoder; this workspace implements that contract in pure Rust for twelve
transfer syntaxes. It mirrors the split between the C# `fo-dicom` and
`fo-dicom.Codecs` packages.

## Highlights

- **Twelve transfer syntaxes**: RLE Lossless, classic JPEG, JPEG-LS, JPEG 2000 and HTJ2K.
- **Pure Rust**: no native libraries, no FFI and no vendored third-party codec source.
- **One registration call**: `radicom_codecs::install()` wires every codec into
  `radicom`'s process-wide registry, so its transcoder resolves them by UID.
- **Interop-checked**: validated against `fo-dicom.Codecs` 5.16.7 and OpenJPEG/OpenJPH
  reference output; see the [design docs](docs/design/radicom-codecs-design.md).

## Installation

```toml
[dependencies]
radicom = "0.1"
radicom-codecs = "0.1"
```

The facade crate re-exports every codec type and owns registration. Depend on a
family crate directly only when you want its codec types without the facade.

## Quick start

```rust
use radicom::io::file::DicomFile;
use radicom::model::transfer_syntax::syntaxes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Register all twelve codecs in the process-wide radicom registry once.
    radicom_codecs::install();

    // Re-encode an existing DICOM file as JPEG 2000 Lossless.
    let file = DicomFile::open("input.dcm")?;
    let j2k = file.clone_with_syntax(syntaxes::JPEG_2000_LOSSLESS.clone(), None)?;
    j2k.save("output.dcm")?;
    Ok(())
}
```

`install()` returns the registry it installed. To build a registry without touching
global state, use `radicom_codecs::codec_registry()`.

## Supported transfer syntaxes

| Transfer syntax | UID | Crate |
|---|---|---|
| RLE Lossless | `1.2.840.10008.1.2.5` | `radicom-codecs-rle` |
| JPEG Baseline (Process 1) | `1.2.840.10008.1.2.4.50` | `radicom-codecs-jpeg` |
| JPEG Extended (Process 2 & 4) | `1.2.840.10008.1.2.4.51` | `radicom-codecs-jpeg` |
| JPEG Lossless (Process 14) | `1.2.840.10008.1.2.4.57` | `radicom-codecs-jpeg` |
| JPEG Lossless (Process 14 SV1) | `1.2.840.10008.1.2.4.70` | `radicom-codecs-jpeg` |
| JPEG-LS Lossless | `1.2.840.10008.1.2.4.80` | `radicom-codecs-jpeg-ls` |
| JPEG-LS Near-Lossless | `1.2.840.10008.1.2.4.81` | `radicom-codecs-jpeg-ls` |
| JPEG 2000 Lossless | `1.2.840.10008.1.2.4.90` | `radicom-codecs-jpeg2000` |
| JPEG 2000 Lossy | `1.2.840.10008.1.2.4.91` | `radicom-codecs-jpeg2000` |
| HTJ2K Lossless | `1.2.840.10008.1.2.4.201` | `radicom-codecs-jpeg2000` |
| HTJ2K Lossless RPCL | `1.2.840.10008.1.2.4.202` | `radicom-codecs-jpeg2000` |
| HTJ2K Lossy | `1.2.840.10008.1.2.4.203` | `radicom-codecs-jpeg2000` |

## Workspace layout

| Crate | Contents |
|---|---|
| **`radicom-codecs`** | Public facade: registry, one-call registration and re-exports |
| **`radicom-codecs-rle`** | RLE Lossless |
| **`radicom-codecs-jpeg`** | Classic JPEG (Process 1, Process 2/4, Lossless 14, Lossless 14 SV1) |
| **`radicom-codecs-jpeg-ls`** | JPEG-LS Lossless and Near-Lossless |
| **`radicom-codecs-jpeg2000`** | Classic JPEG 2000 and HTJ2K |
| **`radicom-codecs-tool`** | `radicom-compress` CLI; see its [README](crates/radicom-codecs-tool/README.md) |

## Requirements

Rust 1.89 or newer. No C/C++ toolchain, CMake or native codec library is required.

## CLI

`radicom-compress` writes compressed DICOM copies for any subset of the twelve
transfer syntaxes:

```powershell
cargo install radicom-codecs-tool
radicom-compress study.dcm --output-dir out --format j2k_lossless
```

See [the tool README](crates/radicom-codecs-tool/README.md) for format suffixes and
behavior notes.

## Development

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI runs these gates on `master`/`main` for pushes and pull requests, plus a
`cargo package` metadata check and an MSRV build. Pushing a `v<version>` tag runs
the release workflow: it verifies the tag against every crate version, re-runs the
gates, publishes the crates to crates.io in dependency order, and creates a GitHub
release whose notes compare the previous and current version. The version-bump,
tagging, and recovery steps are in
[docs/development/releasing.md](docs/development/releasing.md).

## License

MIT. See [LICENSE](LICENSE).
