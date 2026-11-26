//! Compression-speed benchmark across the twelve transfer syntaxes.
//!
//! Times `Dataset::clone_with_syntax` — the encode path `radicom-compress` uses
//! — plus the matching decode, on deterministic synthetic images. Reports the
//! median encode/decode milliseconds, throughput in MB/s of raw pixel data, and
//! the compression ratio.
//!
//! Release builds only; this is a development tool, not part of CI:
//!
//!   cargo run --release -p radicom-codecs-tool --bin radicom-bench

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use radicom::io::file::DicomFile;
use radicom::model::dataset::Dataset;
use radicom::model::tag::Tag;
use radicom::model::transfer_syntax::syntaxes;
use radicom::model::vr::Vr;
use radicom::pixel::{PixelData, PixelDataRef};

use radicom_codecs_tool::{params_for, select_formats, TargetFormat};

const DEFAULT_RUNS: u32 = 5;
const MEDIUM_CASE: Duration = Duration::from_millis(500);
const SLOW_CASE: Duration = Duration::from_millis(2_000);

struct BenchImage {
    name: String,
    dataset: Dataset,
    raw_bytes: usize,
    samples: u16,
}

struct Spec {
    name: &'static str,
    rows: u16,
    cols: u16,
    bits_allocated: u16,
    bits_stored: u16,
    samples: u16,
    photometric: &'static str,
    signed: bool,
}

const SPECS: [Spec; 5] = [
    Spec {
        name: "CT 512x512 16/16",
        rows: 512,
        cols: 512,
        bits_allocated: 16,
        bits_stored: 16,
        samples: 1,
        photometric: "MONOCHROME2",
        signed: true,
    },
    Spec {
        name: "CR 2048x2048 16/12",
        rows: 2048,
        cols: 2048,
        bits_allocated: 16,
        bits_stored: 12,
        samples: 1,
        photometric: "MONOCHROME2",
        signed: false,
    },
    Spec {
        name: "MG 3328x4096 16/12",
        rows: 3328,
        cols: 4096,
        bits_allocated: 16,
        bits_stored: 12,
        samples: 1,
        photometric: "MONOCHROME2",
        signed: false,
    },
    Spec {
        name: "US 640x480 8/8 RGB",
        rows: 480,
        cols: 640,
        bits_allocated: 8,
        bits_stored: 8,
        samples: 3,
        photometric: "RGB",
        signed: false,
    },
    Spec {
        name: "SC 1024x768 8/8",
        rows: 768,
        cols: 1024,
        bits_allocated: 8,
        bits_stored: 8,
        samples: 1,
        photometric: "MONOCHROME2",
        signed: false,
    },
];

enum Parsed {
    Help,
    Run(Options),
}

struct Options {
    runs: u32,
    corpus: Option<PathBuf>,
    dump: Option<PathBuf>,
    targets: Vec<TargetFormat>,
}

enum Outcome {
    Unsupported(String),
    Measured(Measurement),
}

struct Measurement {
    encode_ms: f64,
    encode_mbps: f64,
    ratio: f64,
    decode_ms: f64,
    decode_mbps: f64,
    check: String,
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("error: this benchmark is meaningless in a debug build; run it with --release");
        std::process::exit(1);
    }
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args)? {
        Parsed::Help => {
            print_usage();
            return Ok(());
        }
        Parsed::Run(options) => options,
    };
    radicom_codecs::install();

    let images = match &options.corpus {
        Some(dir) => corpus_images(dir)?,
        None => synthetic_images(),
    };
    if let Some(dir) = &options.dump {
        dump_raw_frames(dir, &images)?;
        println!();
    }

    println!(
        "machine: {} {}, {} logical cores, release",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1),
    );
    println!(
        "runs: up to {} per case (median), warm-up excluded",
        options.runs
    );
    println!();
    print_header();
    for image in &images {
        for target in &options.targets {
            let outcome = measure(image, target, options.runs);
            print_row(image, target, &outcome);
        }
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut runs = DEFAULT_RUNS;
    let mut corpus = None;
    let mut dump = None;
    let mut format = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--help" | "-h" | "/?" => return Ok(Parsed::Help),
            "--runs" => {
                index += 1;
                let value = args.get(index).ok_or("--runs needs a count")?;
                runs = value
                    .parse()
                    .map_err(|_| format!("invalid --runs value '{value}'"))?;
            }
            "--corpus" => {
                index += 1;
                corpus = Some(PathBuf::from(
                    args.get(index).ok_or("--corpus needs a directory")?,
                ));
            }
            "--dump" => {
                index += 1;
                dump = Some(PathBuf::from(
                    args.get(index).ok_or("--dump needs a directory")?,
                ));
            }
            "--format" => {
                index += 1;
                format = Some(args.get(index).ok_or("--format needs a suffix")?.clone());
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
        index += 1;
    }
    if runs == 0 {
        return Err("--runs must be at least 1".to_owned());
    }
    let targets = select_formats(format.as_deref()).map_err(|error| error.to_string())?;
    Ok(Parsed::Run(Options {
        runs,
        corpus,
        dump,
        targets,
    }))
}

fn print_usage() {
    println!("Usage:");
    println!("  radicom-bench [--runs N] [--format SUFFIX] [--corpus DIR] [--dump DIR]");
    println!();
    println!("  --runs N        timed runs per case, up to (default {DEFAULT_RUNS})");
    println!("  --format NAME   benchmark one transfer syntax by suffix, as radicom-compress does");
    println!(
        "  --corpus DIR    read uncompressed .dcm files from DIR instead of generating images"
    );
    println!("  --dump DIR      write the raw pixel frames to DIR for external codec comparisons");
    println!();
    println!("Times Dataset::clone_with_syntax (encode) and the matching decode;");
    println!("file I/O and process startup are excluded. Release builds only.");
}

fn synthetic_images() -> Vec<BenchImage> {
    SPECS.iter().map(bench_image).collect()
}

fn bench_image(spec: &Spec) -> BenchImage {
    let pixels = generate_pixels(
        spec.rows,
        spec.cols,
        spec.bits_stored,
        spec.samples,
        spec.signed,
    );
    let dataset = dataset_for(spec, &pixels);
    BenchImage {
        name: spec.name.to_owned(),
        dataset,
        raw_bytes: pixels.len(),
        samples: spec.samples,
    }
}

fn dataset_for(spec: &Spec, pixels: &[u8]) -> Dataset {
    let mut dataset = Dataset::new();
    dataset.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    for (tag, value) in [
        (Tag::ROWS, spec.rows),
        (Tag::COLUMNS, spec.cols),
        (Tag::BITS_ALLOCATED, spec.bits_allocated),
        (Tag::BITS_STORED, spec.bits_stored),
        (Tag::HIGH_BIT, spec.bits_stored - 1),
        (Tag::SAMPLES_PER_PIXEL, spec.samples),
        (Tag::PIXEL_REPRESENTATION, u16::from(spec.signed)),
    ] {
        dataset.insert_value(tag, value);
    }
    dataset.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, spec.photometric);
    if spec.samples > 1 {
        dataset.insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
    }
    PixelData::create_new(&mut dataset)
        .expect("create pixel data")
        .add_frame(pixels)
        .expect("add frame");
    dataset
}

fn corpus_images(dir: &Path) -> Result<Vec<BenchImage>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dcm"))
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(format!("no .dcm files in {}", dir.display()));
    }

    let mut images = Vec::new();
    for path in paths {
        let file =
            DicomFile::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        if file.dataset.transfer_syntax().is_encapsulated() {
            return Err(format!(
                "{}: compressed source; provide an uncompressed DICOM file",
                path.display()
            ));
        }
        let pixel = PixelDataRef::from_dataset(&file.dataset)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if pixel.number_of_frames() > 1 {
            return Err(format!(
                "{}: multi-frame sources are not supported yet",
                path.display()
            ));
        }
        let samples = pixel.samples_per_pixel();
        let raw_bytes = native_bytes(&file.dataset)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .len();
        images.push(BenchImage {
            name: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dataset: file.dataset,
            raw_bytes,
            samples,
        });
    }
    Ok(images)
}

fn dump_raw_frames(dir: &Path, images: &[BenchImage]) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    for image in images {
        let name: String = image
            .name
            .chars()
            .map(|value| {
                if value.is_ascii_alphanumeric() {
                    value
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("{name}.raw"));
        let bytes =
            native_bytes(&image.dataset).map_err(|error| format!("{}: {error}", image.name))?;
        std::fs::write(&path, bytes)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        println!("dumped {}", path.display());
    }
    Ok(())
}

fn native_bytes(dataset: &Dataset) -> Result<Vec<u8>, String> {
    let pixel = PixelDataRef::from_dataset(dataset).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    for index in 0..pixel.number_of_frames() {
        let frame = pixel.get_frame(index).map_err(|error| error.to_string())?;
        bytes.extend_from_slice(&frame.data().map_err(|error| error.to_string())?);
    }
    Ok(bytes)
}

fn measure(image: &BenchImage, target: &TargetFormat, runs: u32) -> Outcome {
    let params = params_for(target);
    let syntax = target.syntax.clone();
    let started = Instant::now();
    let encoded = match image
        .dataset
        .clone_with_syntax(syntax.clone(), params.clone())
    {
        Ok(encoded) => encoded,
        Err(error) => {
            let reason = error.to_string();
            let first = reason.lines().next().unwrap_or_default();
            return Outcome::Unsupported(first.to_owned());
        }
    };
    let first_case = started.elapsed();
    let runs = if first_case >= SLOW_CASE {
        1
    } else if first_case >= MEDIUM_CASE {
        3
    } else {
        runs
    };

    let mut encode_times = Vec::with_capacity(runs as usize);
    for _ in 0..runs {
        let started = Instant::now();
        if let Err(error) = image
            .dataset
            .clone_with_syntax(syntax.clone(), params.clone())
        {
            return Outcome::Unsupported(error.to_string());
        }
        encode_times.push(started.elapsed());
    }
    let encode = median(&mut encode_times);

    let raw_bytes = image.raw_bytes as f64;
    let compressed = payload_bytes(&encoded).unwrap_or(0) as f64;
    let ratio = if raw_bytes > 0.0 {
        compressed / raw_bytes
    } else {
        0.0
    };

    let native = syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone();
    let mut decode_times = Vec::with_capacity(runs as usize);
    let mut decoded = None;
    for _ in 0..runs {
        let started = Instant::now();
        match encoded.clone_with_syntax(native.clone(), None) {
            Ok(dataset) => {
                decode_times.push(started.elapsed());
                decoded = Some(dataset);
            }
            Err(error) => return Outcome::Unsupported(format!("decode failed: {error}")),
        }
    }
    let decode = median(&mut decode_times);
    let check = match (&decoded, image.samples) {
        (Some(dataset), 1) => verify_lossless(image, target, dataset),
        _ => "-".to_owned(),
    };

    Outcome::Measured(Measurement {
        encode_ms: millis(encode),
        encode_mbps: throughput(raw_bytes, encode),
        ratio,
        decode_ms: millis(decode),
        decode_mbps: throughput(raw_bytes, decode),
        check,
    })
}

fn payload_bytes(dataset: &Dataset) -> Result<usize, String> {
    let pixel = PixelDataRef::from_dataset(dataset).map_err(|error| error.to_string())?;
    let mut total = 0;
    for index in 0..pixel.number_of_frames() {
        let frame = pixel.get_frame(index).map_err(|error| error.to_string())?;
        total += frame.data().map_err(|error| error.to_string())?.len();
    }
    Ok(total)
}

fn verify_lossless(image: &BenchImage, target: &TargetFormat, decoded: &Dataset) -> String {
    if target.syntax.is_lossy() {
        return "lossy".to_owned();
    }
    let source = match native_bytes(&image.dataset) {
        Ok(bytes) => bytes,
        Err(error) => return format!("source error: {error}"),
    };
    let actual = match native_bytes(decoded) {
        Ok(bytes) => bytes,
        Err(error) => return format!("decode error: {error}"),
    };
    if source == actual {
        "exact".to_owned()
    } else {
        "MISMATCH".to_owned()
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn throughput(raw_bytes: f64, duration: Duration) -> f64 {
    if duration.is_zero() {
        0.0
    } else {
        raw_bytes / duration.as_secs_f64() / 1_000_000.0
    }
}

fn median(times: &mut [Duration]) -> Duration {
    if times.is_empty() {
        return Duration::ZERO;
    }
    times.sort_unstable();
    let middle = times.len() / 2;
    if times.len().is_multiple_of(2) {
        (times[middle - 1] + times[middle]) / 2
    } else {
        times[middle]
    }
}

fn print_header() {
    println!(
        "{:<24} {:<30} {:>9} {:>7} {:>6} {:>9} {:>7}  check",
        "image", "syntax", "enc ms", "MB/s", "ratio", "dec ms", "MB/s"
    );
    println!("{}", "-".repeat(108));
}

fn print_row(image: &BenchImage, target: &TargetFormat, outcome: &Outcome) {
    match outcome {
        Outcome::Unsupported(reason) => println!(
            "{:<24.24} {:<30.30} unsupported: {reason}",
            image.name, target.name
        ),
        Outcome::Measured(measurement) => println!(
            "{:<24.24} {:<30.30} {:>9.1} {:>7.1} {:>6.2} {:>9.1} {:>7.1}  {}",
            image.name,
            target.name,
            measurement.encode_ms,
            measurement.encode_mbps,
            measurement.ratio,
            measurement.decode_ms,
            measurement.decode_mbps,
            measurement.check
        ),
    }
}

/// Deterministic synthetic pixels: a smooth low-frequency field, integer noise
/// and a few hard-edged structures. Integer-only math keeps every platform on
/// the same bytes, so runs remain comparable across machines and commits.
fn generate_pixels(rows: u16, cols: u16, bits_stored: u16, samples: u16, signed: bool) -> Vec<u8> {
    let width = u32::from(cols);
    let height = u32::from(rows);
    let max = (1u32 << bits_stored) - 1;
    let luma = channel(width, height, max, 0x1234_5678_9abc_def0);
    let bytes_per_sample = if bits_stored > 8 { 2 } else { 1 };
    let mut pixels =
        Vec::with_capacity((width * height) as usize * usize::from(samples) * bytes_per_sample);
    let offset = if signed { 1i64 << (bits_stored - 1) } else { 0 };
    let (reddish, bluish) = if samples == 3 {
        (
            channel(width, height, max, 0x0f0e_0d0c_0b0a_0908),
            channel(width, height, max, 0x8877_6655_4433_2211),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    for index in 0..(width * height) as usize {
        let base = luma[index];
        let values: [u32; 3] = if samples == 3 {
            [
                (3 * base + reddish[index]) / 4,
                (3 * base + (reddish[index] + bluish[index]) / 2) / 4,
                (3 * base + bluish[index]) / 4,
            ]
        } else {
            [base, 0, 0]
        };
        for value in values.iter().take(usize::from(samples)) {
            let shifted = i64::from(*value) - offset;
            if bytes_per_sample == 1 {
                pixels.push(shifted.clamp(0, i64::from(max)) as u8);
            } else {
                pixels.extend_from_slice(&(shifted as i16).to_le_bytes());
            }
        }
    }
    pixels
}

fn channel(width: u32, height: u32, max: u32, seed: u64) -> Vec<u32> {
    let grid_width = (width / 16 + 2) as usize;
    let grid_height = (height / 16 + 2) as usize;
    let mut grid_rng = Lcg::new(seed);
    let mut grid = vec![0u32; grid_width * grid_height];
    for value in &mut grid {
        *value = grid_rng.range(max + 1);
    }

    let mut noise = Lcg::new(seed ^ 0x2545_f491_4f6c_dd1d);
    let mut values = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        let row = (y / 16) as usize;
        let fy = y % 16;
        for x in 0..width {
            let column = (x / 16) as usize;
            let fx = x % 16;
            let top = lerp(
                grid[row * grid_width + column],
                grid[row * grid_width + column + 1],
                fx,
            );
            let bottom = lerp(
                grid[(row + 1) * grid_width + column],
                grid[(row + 1) * grid_width + column + 1],
                fx,
            );
            let smooth = lerp(top, bottom, fy);
            let delta = i64::from(noise.range(max / 32 + 1)) - i64::from(max / 64);
            let value = (i64::from(smooth) + delta).clamp(0, i64::from(max)) as u32;
            values.push(value);
        }
    }

    let rects = [
        (width / 8, height / 8, width / 3, height / 2, 0),
        (width / 2, height / 3, width * 3 / 4, height * 2 / 3, max),
        (
            width / 5,
            height * 2 / 3,
            width / 4,
            height * 3 / 4,
            max / 5,
        ),
    ];
    for (x0, y0, x1, y1, value) in rects {
        for y in y0..y1 {
            for x in x0..x1 {
                values[(y * width + x) as usize] = value;
            }
        }
    }
    values
}

fn lerp(a: u32, b: u32, t: u32) -> u32 {
    (i64::from(a) + (i64::from(b) - i64::from(a)) * i64::from(t) / 16) as u32
}

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn range(&mut self, max_exclusive: u32) -> u32 {
        if max_exclusive == 0 {
            0
        } else {
            self.next_u32() % max_exclusive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_pixels_is_deterministic() {
        let first = generate_pixels(24, 32, 12, 1, false);
        let second = generate_pixels(24, 32, 12, 1, false);
        assert_eq!(first, second);
        assert_eq!(first.len(), 24 * 32 * 2);
    }

    #[test]
    fn generate_pixels_lays_out_rgb_samples() {
        let pixels = generate_pixels(16, 16, 8, 3, false);
        assert_eq!(pixels.len(), 16 * 16 * 3);
        assert!(pixels.iter().any(|byte| *byte != 0));
    }

    #[test]
    fn generate_pixels_stays_inside_the_signed_range() {
        let pixels = generate_pixels(8, 8, 12, 1, true);
        for index in (0..pixels.len()).step_by(2) {
            let value = i16::from_le_bytes([pixels[index], pixels[index + 1]]);
            assert!((-2048..=2047).contains(&value));
        }
    }

    #[test]
    fn median_handles_odd_and_even_lengths() {
        let mut odd = [
            Duration::from_millis(3),
            Duration::from_millis(1),
            Duration::from_millis(2),
        ];
        assert_eq!(median(&mut odd), Duration::from_millis(2));

        let mut even = [
            Duration::from_millis(4),
            Duration::from_millis(1),
            Duration::from_millis(3),
            Duration::from_millis(2),
        ];
        assert_eq!(
            median(&mut even),
            Duration::from_millis(2) + Duration::from_micros(500)
        );
    }
}
