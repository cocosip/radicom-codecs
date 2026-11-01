# Interop harnesses

`run-ht-interop.ps1` compares this crate's HTJ2K codecs against fo-dicom's
managed implementation in both directions. `.interop/` holds the binary
artifacts and is not versioned; recreate it before a run:

1. `cargo build -p radicom-codecs-jpeg2000 --example j2k_tool`
2. Recreate `.interop/j2k-oracle` (a `net8.0` console project referencing
   fo-dicom.PureCodecs) and `dotnet build -c Release`. It needs the
   `wrap`, `decode`, `extract` and `ingest` subcommands.
3. `pwsh scripts/interop/run-ht-interop.ps1`

The script prints one line per direction and case, for example
`Rust->fo-dicom mono8 rpcl-tlm ok=True mean=0 max=0`, and writes its scratch
files to `.interop/ht-run`.

`.interop/htj2k-ref` builds reference tables straight from the fo-dicom
sources and is the oracle for the pinned step-size vectors in
`src/htj2k/quant.rs`.
