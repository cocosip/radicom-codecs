//! DICOM compression tool: writes DICOM copies in the twelve transfer
//! syntaxes implemented by `radicom-codecs`.
//!
//! The library half owns the target table and the compression loop; the binary
//! in `main.rs` owns argument parsing and console output.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use radicom::error::Error;
use radicom::io::file::DicomFile;
use radicom::model::transfer_syntax::TransferSyntax;
use radicom::pixel::codec::{CodecParams, J2kParams, JpegParams, SampleFactor};
use radicom::pixel::photometric::PhotometricInterpretation;
use radicom::pixel::{PixelDataRef, PIXEL_DATA};

/// One compression target.
#[derive(Debug, Clone)]
pub struct TargetFormat {
    pub name: &'static str,
    pub suffix: &'static str,
    pub syntax: Arc<TransferSyntax>,
    /// `None` uses the codec's defaults for the target syntax.
    pub params: Option<CodecParams>,
}

fn format(
    name: &'static str,
    suffix: &'static str,
    syntax: Arc<TransferSyntax>,
    params: Option<CodecParams>,
) -> TargetFormat {
    TargetFormat {
        name,
        suffix,
        syntax,
        params,
    }
}

/// The twelve transfer syntaxes this tool writes, in report order.
pub fn target_formats() -> Vec<TargetFormat> {
    use radicom::syntaxes;
    vec![
        format("RLE Lossless", "rle", syntaxes::RLE_LOSSLESS.clone(), None),
        format(
            "JPEG Baseline Process 1",
            "jpeg_baseline",
            syntaxes::JPEG_PROCESS_1.clone(),
            None,
        ),
        format(
            "JPEG Extended Process 2/4",
            "jpeg_process2_4",
            syntaxes::JPEG_PROCESS_2_4.clone(),
            None,
        ),
        format(
            "JPEG Lossless Process 14",
            "jpeg_lossless_14",
            syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
            None,
        ),
        format(
            "JPEG Lossless Process 14 SV1",
            "jpeg_lossless_sv1",
            syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1.clone(),
            None,
        ),
        format(
            "JPEG-LS Lossless",
            "jpegls_lossless",
            syntaxes::JPEG_LS_LOSSLESS.clone(),
            None,
        ),
        format(
            "JPEG-LS Near-Lossless",
            "jpegls_near_lossless",
            syntaxes::JPEG_LS_NEAR_LOSSLESS.clone(),
            None,
        ),
        format(
            "JPEG 2000 Lossless",
            "j2k_lossless",
            syntaxes::JPEG_2000_LOSSLESS.clone(),
            None,
        ),
        format(
            "JPEG 2000 Lossy",
            "j2k_lossy",
            syntaxes::JPEG_2000_LOSSY.clone(),
            // The classic encoder emits one fixed-quality layer; non-default
            // `rate`/`rate_levels` are rejected, so use the codec defaults.
            None,
        ),
        format(
            "HTJ2K Lossless",
            "htj2k_lossless",
            syntaxes::HTJ2K_LOSSLESS.clone(),
            None,
        ),
        format(
            "HTJ2K Lossless RPCL",
            "htj2k_lossless_rpcl",
            syntaxes::HTJ2K_LOSSLESS_RPCL.clone(),
            None,
        ),
        format(
            "HTJ2K Lossy",
            "htj2k_lossy",
            syntaxes::HTJ2K_LOSSY.clone(),
            None,
        ),
    ]
}

/// Result of one target format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    /// The target syntax cannot represent this image, so it was skipped
    /// without writing a file.
    Unsupported,
    Failed,
}

/// Result of compressing the input to one target format.
#[derive(Debug, Clone)]
pub struct CompressionOutcome {
    pub format_name: &'static str,
    pub suffix: &'static str,
    pub transfer_syntax_uid: String,
    pub output_path: PathBuf,
    pub status: Status,
    /// Bytes written; `0` when the target failed.
    pub output_size: u64,
    /// Compression note, "already `<format>`; copied", or the failure text.
    pub message: String,
}

/// Compresses `file` to every target format, or to the single `format` suffix.
///
/// `input` supplies the default output directory, the output file stem and the
/// input size in the compression ratio. A target syntax equal to the source
/// syntax is saved without decoding and re-encoding, so lossy input is never
/// compressed twice. A failed target does not stop the remaining formats.
pub fn compress(
    file: &DicomFile,
    input: &Path,
    output_dir: Option<&Path>,
    format: Option<&str>,
) -> Result<Vec<CompressionOutcome>, Error> {
    radicom_codecs::install();

    if !file.dataset.contains(&PIXEL_DATA) {
        return Err(Error::other("input has no pixel data"));
    }

    let targets = select_formats(format)?;
    let output_dir = resolve_output_dir(input, output_dir);
    std::fs::create_dir_all(&output_dir)?;

    let input_size = std::fs::metadata(input)?.len();
    let source_uid = file.dataset.transfer_syntax().uid().to_owned();
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::other("input file name has no usable stem"))?;

    let pixel = PixelDataRef::from_dataset(&file.dataset)?;
    let mut outcomes = Vec::with_capacity(targets.len());
    for target in targets {
        let output_path = output_dir.join(format!("{stem}_{}.dcm", target.suffix));
        if let Some(reason) = unsupported_reason(&target, &pixel) {
            outcomes.push(CompressionOutcome {
                format_name: target.name,
                suffix: target.suffix,
                transfer_syntax_uid: target.syntax.uid().to_owned(),
                output_path,
                status: Status::Unsupported,
                output_size: 0,
                message: reason,
            });
            continue;
        }
        let params = effective_params(&target, &pixel);
        let (status, output_size, message) = match transcode_one(
            file,
            &source_uid,
            target.syntax.clone(),
            params,
            &output_path,
        ) {
            Ok(already) => {
                let output_size = std::fs::metadata(&output_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let message = if already {
                    format!("already {}; copied", target.name)
                } else if output_size > 0 {
                    format!("{:.2}x compression", input_size as f64 / output_size as f64)
                } else {
                    String::new()
                };
                (Status::Success, output_size, message)
            }
            Err(error) => (Status::Failed, 0, error.to_string()),
        };
        outcomes.push(CompressionOutcome {
            format_name: target.name,
            suffix: target.suffix,
            transfer_syntax_uid: target.syntax.uid().to_owned(),
            output_path,
            status,
            output_size,
            message,
        });
    }
    Ok(outcomes)
}

/// Targets this image cannot be encoded to are skipped before transcoding, the
/// way the fo-dicom reference tool pre-classifies them, so no partial or failed
/// file is written. Only the JPEG sequential family has a shape matrix worth
/// rejecting up front; every other target reports its own error.
fn unsupported_reason(target: &TargetFormat, pixel: &PixelDataRef<'_>) -> Option<String> {
    let process2_4 = target.suffix == "jpeg_process2_4";
    if !process2_4 && target.suffix != "jpeg_baseline" {
        return None;
    }
    let Ok(bits_allocated) = pixel.bits_allocated() else {
        return None;
    };
    let bits_stored = pixel.bits_stored();
    let samples = pixel.samples_per_pixel();
    // Process 2/4 carries the 12-bit monochrome case that Process 1 cannot.
    if process2_4
        && bits_allocated == 16
        && bits_stored == 12
        && samples == 1
        && matches!(
            pixel.photometric_interpretation(),
            Some(PhotometricInterpretation::Monochrome1 | PhotometricInterpretation::Monochrome2)
        )
    {
        return None;
    }
    if bits_allocated != 8 || bits_stored != 8 {
        return Some(format!(
            "JPEG sequential DCT supports only BitsAllocated 8 and BitsStored 8; this image has BitsAllocated={bits_allocated}, BitsStored={bits_stored}."
        ));
    }
    if samples != 1 && samples != 3 {
        return Some("JPEG sequential DCT supports only SamplesPerPixel 1 or 3.".to_owned());
    }
    match pixel.photometric_interpretation() {
        Some(
            PhotometricInterpretation::Monochrome1
            | PhotometricInterpretation::Monochrome2
            | PhotometricInterpretation::PaletteColor
            | PhotometricInterpretation::Rgb
            | PhotometricInterpretation::YbrFull
            | PhotometricInterpretation::YbrFull422,
        ) => None,
        other => Some(format!(
            "JPEG sequential DCT does not support photometric interpretation {}.",
            other
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<missing>".to_owned())
        )),
    }
}

/// Parameters that make the tool's output match the fo-dicom reference: 4:2:2
/// for colour sequential JPEG, and the component transform for JPEG 2000.
fn effective_params(target: &TargetFormat, pixel: &PixelDataRef<'_>) -> Option<CodecParams> {
    if target.params.is_some() {
        return target.params.clone();
    }
    match target.suffix {
        "jpeg_baseline" | "jpeg_process2_4" if pixel.samples_per_pixel() == 3 => {
            Some(CodecParams::Jpeg(JpegParams {
                sample_factor: SampleFactor::Half,
                ..Default::default()
            }))
        }
        "j2k_lossless" => Some(CodecParams::Jpeg2000(J2kParams {
            irreversible: false,
            allow_mct: true,
            update_photometric_interpretation: true,
            ..Default::default()
        })),
        "j2k_lossy" => Some(CodecParams::Jpeg2000(J2kParams {
            // The reference tool requests a ratio of 16 for the lossy syntax.
            rate: 16,
            allow_mct: true,
            update_photometric_interpretation: true,
            ..Default::default()
        })),
        _ => None,
    }
}

/// Transcodes one target; returns `true` when the source already used it.
fn transcode_one(
    file: &DicomFile,
    source_uid: &str,
    syntax: Arc<TransferSyntax>,
    params: Option<CodecParams>,
    output_path: &Path,
) -> Result<bool, Error> {
    if source_uid == syntax.uid() {
        file.clone().save(output_path)?;
        return Ok(true);
    }
    let transcoded = file.clone_with_syntax(syntax, params)?;
    transcoded.save(output_path)?;
    Ok(false)
}

/// Every target, or the one whose suffix matches `format` (case-insensitive).
fn select_formats(format: Option<&str>) -> Result<Vec<TargetFormat>, Error> {
    let all = target_formats();
    let Some(suffix) = format else {
        return Ok(all);
    };
    let suffix = suffix.trim();
    let selected: Vec<TargetFormat> = all
        .iter()
        .filter(|target| target.suffix.eq_ignore_ascii_case(suffix))
        .cloned()
        .collect();
    if selected.is_empty() {
        let known: Vec<&str> = all.iter().map(|target| target.suffix).collect();
        return Err(Error::other(format!(
            "unknown format '{suffix}' (known formats: {})",
            known.join(", ")
        )));
    }
    Ok(selected)
}

/// `<input dir>/<stem>_compressed` unless the caller supplied a directory.
fn resolve_output_dir(input: &Path, output_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = output_dir {
        return dir.to_path_buf();
    }
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("output");
    match input.parent() {
        Some(parent) => parent.join(format!("{stem}_compressed")),
        None => PathBuf::from(format!("{stem}_compressed")),
    }
}
