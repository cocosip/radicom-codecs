//! `radicom-compress` — compress a DICOM file into every transfer syntax
//! implemented by `radicom-codecs`, or into one selected format.
//!
//! Usage:
//!   radicom-compress `<input-file>` [--output-dir `<directory>`] [--format `<suffix>`]

use std::path::PathBuf;

use radicom::io::file::DicomFile;
use radicom::pixel::PixelDataRef;

use radicom_codecs_tool::{compress, target_formats, CompressionOutcome, Status};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

enum Parsed {
    Help,
    Run(RunOptions),
}

struct RunOptions {
    input: PathBuf,
    output_dir: Option<PathBuf>,
    format: Option<String>,
}

/// Runs the CLI with `args` (the process arguments after `argv[0]`) and returns
/// the process exit code, so argument handling is testable without a process.
fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        print_usage();
        return 1;
    }

    let options = match parse(args) {
        Ok(Parsed::Help) => {
            print_usage();
            return 0;
        }
        Ok(Parsed::Run(options)) => options,
        Err(message) => {
            eprintln!("error: {message}");
            print_usage();
            return 1;
        }
    };

    let file = match DicomFile::open(&options.input) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("error: cannot open {}: {error}", options.input.display());
            return 1;
        }
    };

    println!("DICOM compression tool");
    println!("Input: {}", options.input.display());
    print_image_info(&file);

    let outcomes = match compress(
        &file,
        &options.input,
        options.output_dir.as_deref(),
        options.format.as_deref(),
    ) {
        Ok(outcomes) => outcomes,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    if let Some(dir) = outcomes
        .first()
        .and_then(|outcome| outcome.output_path.parent())
    {
        println!("Output directory: {}", dir.display());
    }
    println!();
    print_outcomes(&outcomes);

    let succeeded = outcomes
        .iter()
        .filter(|outcome| outcome.status == Status::Success)
        .count();
    let failed = outcomes.len() - succeeded;
    println!();
    println!("Summary: {succeeded} succeeded, {failed} failed.");
    if succeeded == 0 {
        1
    } else {
        0
    }
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut input: Option<PathBuf> = None;
    let mut output_dir: Option<PathBuf> = None;
    let mut format: Option<String> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--help" | "-h" | "/?" => return Ok(Parsed::Help),
            "--output-dir" | "-o" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or("--output-dir requires a directory path")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--format" => {
                index += 1;
                let value = args.get(index).ok_or("--format requires a format name")?;
                format = Some(value.clone());
            }
            argument if argument.starts_with('-') => {
                return Err(format!("unknown option {argument}"));
            }
            argument => {
                if input.is_some() {
                    return Err("only one input file can be compressed at a time".to_owned());
                }
                input = Some(PathBuf::from(argument));
            }
        }
        index += 1;
    }

    let input = input.ok_or("missing input file")?;
    Ok(Parsed::Run(RunOptions {
        input,
        output_dir,
        format,
    }))
}

fn print_image_info(file: &DicomFile) {
    let Ok(pixel) = PixelDataRef::from_dataset(&file.dataset) else {
        return;
    };
    println!("Image:");
    println!("    Rows: {}", pixel.height());
    println!("    Columns: {}", pixel.width());
    println!("    Bits Stored: {}", pixel.bits_stored());
    println!("    Samples Per Pixel: {}", pixel.samples_per_pixel());
    if let Some(photometric) = pixel.photometric_interpretation() {
        println!("    Photometric Interpretation: {photometric}");
    }
    println!(
        "    Source Transfer Syntax: {}",
        pixel.transfer_syntax().uid()
    );
}

fn print_outcomes(outcomes: &[CompressionOutcome]) {
    for (index, outcome) in outcomes.iter().enumerate() {
        println!("[{}/{}] {}", index + 1, outcomes.len(), outcome.format_name);
        println!("    Transfer Syntax: {}", outcome.transfer_syntax_uid);
        match outcome.status {
            Status::Success => {
                let name = outcome
                    .output_path
                    .file_name()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("    Success: {name}");
                println!(
                    "    Size: {} ({})",
                    format_bytes(outcome.output_size),
                    outcome.message
                );
            }
            Status::Failed => println!("    Failed: {}", outcome.message),
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn print_usage() {
    println!("Usage:");
    println!("  radicom-compress <input-file> [--output-dir <directory>] [--format <format>]");
    println!();
    println!("Writes one compressed DICOM copy per target transfer syntax into");
    println!("<input-stem>_compressed/ next to the input unless --output-dir is given.");
    let suffixes: Vec<&str> = target_formats()
        .iter()
        .map(|target| target.suffix)
        .collect();
    println!("Formats: {}", suffixes.join(", "));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn help_exits_with_0() {
        assert_eq!(run(&args(&["--help"])), 0);
        assert_eq!(run(&args(&["-h"])), 0);
        assert_eq!(run(&args(&["/?"])), 0);
    }

    #[test]
    fn empty_arguments_exit_with_1() {
        assert_eq!(run(&[]), 1);
    }

    #[test]
    fn parsing_collects_input_output_and_format() {
        match parse(&args(&["in.dcm", "-o", "out", "--format", "rle"])).unwrap() {
            Parsed::Run(options) => {
                assert_eq!(options.input, PathBuf::from("in.dcm"));
                assert_eq!(options.output_dir, Some(PathBuf::from("out")));
                assert_eq!(options.format.as_deref(), Some("rle"));
            }
            Parsed::Help => panic!("expected run options"),
        }
    }

    #[test]
    fn unknown_options_and_missing_values_are_rejected() {
        assert!(parse(&args(&["--nope"])).is_err());
        assert!(parse(&args(&["--format"])).is_err());
        assert!(parse(&args(&["--output-dir"])).is_err());
        assert!(parse(&args(&["a.dcm", "b.dcm"])).is_err());
        assert!(parse(&[]).is_err());
    }
}
