//! Interop helper: encode a raw frame to a compressed codestream for any
//! registered transfer syntax, or decode a codestream back to raw pixels. Not
//! part of the shipped library; it exists so the external interop matrix can
//! drive every codec with the same command line as the reference tooling.

use std::collections::HashMap;
use std::sync::Arc;

use radicom::model::{
    buffer::MemoryBuffer,
    dataset::Dataset,
    tag::Tag,
    transfer_syntax::{self, syntaxes, TransferSyntax},
    vr::Vr,
};
use radicom::pixel::{PixelData, PixelDataRef};

fn main() {
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    radicom_codecs::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().ok_or("usage: codec_tool encode|decode ...")?;
    let options = parse(&args[1..])?;
    match command.as_str() {
        "encode" => encode(&options),
        "decode" => decode(&options),
        other => Err(format!("unknown command '{other}'")),
    }
}

fn parse(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut options = HashMap::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index]
            .strip_prefix("--")
            .ok_or_else(|| format!("expected --flag, got '{}'", args[index]))?;
        if key == "signed" {
            options.insert(key.to_owned(), "true".to_owned());
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value for --{key}"))?;
        options.insert(key.to_owned(), value.clone());
        index += 2;
    }
    Ok(options)
}

fn required<'a>(options: &'a HashMap<String, String>, key: &str) -> Result<&'a str, String> {
    options
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("missing --{key}"))
}

fn number(options: &HashMap<String, String>, key: &str) -> Result<u16, String> {
    required(options, key)?
        .parse()
        .map_err(|_| format!("invalid --{key}"))
}

fn syntax(options: &HashMap<String, String>) -> Result<Arc<TransferSyntax>, String> {
    let uid = required(options, "syntax")?;
    transfer_syntax::query(uid).ok_or_else(|| format!("unknown transfer syntax '{uid}'"))
}

fn base_dataset(options: &HashMap<String, String>) -> Result<Dataset, String> {
    let rows: u16 = number(options, "rows")?;
    let cols: u16 = number(options, "cols")?;
    let bits: u16 = number(options, "bits")?;
    let stored = match options.get("stored") {
        Some(value) => value.parse().map_err(|_| "invalid --stored".to_string())?,
        None => bits,
    };
    let samples: u16 = number(options, "samples")?;
    let signed = options.contains_key("signed");
    let photometric = required(options, "photometric")?;
    let mut dataset = Dataset::new();
    dataset.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    for (tag, value) in [
        (Tag::ROWS, rows),
        (Tag::COLUMNS, cols),
        (Tag::BITS_ALLOCATED, bits),
        (Tag::BITS_STORED, stored),
        (Tag::HIGH_BIT, stored - 1),
        (Tag::SAMPLES_PER_PIXEL, samples),
        (Tag::PIXEL_REPRESENTATION, u16::from(signed)),
    ] {
        dataset.insert_value(tag, value);
    }
    dataset.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, photometric);
    if samples > 1 {
        dataset.insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
    }
    Ok(dataset)
}

fn frame_bytes(dataset: &Dataset) -> Result<Vec<u8>, String> {
    Ok(PixelDataRef::from_dataset(dataset)
        .map_err(|e| e.to_string())?
        .get_frame(0)
        .map_err(|e| e.to_string())?
        .data()
        .map_err(|e| e.to_string())?
        .to_vec())
}

fn encode(options: &HashMap<String, String>) -> Result<(), String> {
    let raw = std::fs::read(required(options, "raw")?).map_err(|e| e.to_string())?;
    let target = syntax(options)?;
    if !target.is_encapsulated() {
        return Err("encode target must be an encapsulated transfer syntax".to_string());
    }
    let mut source = base_dataset(options)?;
    PixelData::create_new(&mut source)
        .map_err(|e| e.to_string())?
        .add_frame(&raw)
        .map_err(|e| e.to_string())?;
    let encoded = source
        .clone_with_syntax(target, None)
        .map_err(|e| e.to_string())?;
    std::fs::write(required(options, "out")?, frame_bytes(&encoded)?).map_err(|e| e.to_string())
}

fn decode(options: &HashMap<String, String>) -> Result<(), String> {
    let codestream = std::fs::read(required(options, "in")?).map_err(|e| e.to_string())?;
    let source_syntax = syntax(options)?;
    let mut source = base_dataset(options)?;
    source.set_transfer_syntax(source_syntax);
    {
        let fragments = source.insert_fragments(Tag::PIXEL_DATA, Vr::Ob);
        fragments.push_fragment_raw(Arc::new(MemoryBuffer::from(codestream)));
        fragments.set_offset_table(vec![0]);
    }
    let native = source
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .map_err(|e| e.to_string())?;
    std::fs::write(required(options, "out")?, frame_bytes(&native)?).map_err(|e| e.to_string())
}
