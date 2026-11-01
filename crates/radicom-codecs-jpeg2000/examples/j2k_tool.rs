//! Small interop helper: encode a raw frame to a JPEG 2000 codestream, or
//! decode a codestream into raw pixels. Not part of the shipped library.

use std::collections::HashMap;
use std::sync::Arc;

use radicom::model::{
    buffer::MemoryBuffer, dataset::Dataset, tag::Tag, transfer_syntax::syntaxes, vr::Vr,
};
use radicom::pixel::codec::{Codec, CodecParams, ProgressionOrder};
use radicom::pixel::{PixelData, PixelDataRef};
use radicom_codecs_jpeg2000::{
    HtJpeg2000LosslessCodec, HtJpeg2000LosslessRpclCodec, HtJpeg2000LossyCodec,
    Jpeg2000LosslessCodec, Jpeg2000LossyCodec,
};

fn main() {
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().ok_or("usage: j2k_tool encode|decode ...")?;
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
        if key == "reversible" || key == "signed" || key == "ht" || key == "rpcl" || key == "tlm" {
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

fn number<T: std::str::FromStr>(options: &HashMap<String, String>, key: &str) -> Result<T, String> {
    required(options, key)?
        .parse()
        .map_err(|_| format!("invalid --{key}"))
}

fn base_dataset(options: &HashMap<String, String>, encapsulated: bool) -> Result<Dataset, String> {
    let rows: u16 = number(options, "rows")?;
    let cols: u16 = number(options, "cols")?;
    let bits: u16 = number(options, "bits")?;
    let samples: u16 = number(options, "samples")?;
    let signed = options.contains_key("signed");
    let photometric = required(options, "photometric")?;
    let mut dataset = Dataset::new();
    dataset.set_transfer_syntax(if encapsulated {
        syntaxes::JPEG_2000_LOSSLESS.clone()
    } else {
        syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone()
    });
    for (tag, value) in [
        (Tag::ROWS, rows),
        (Tag::COLUMNS, cols),
        (Tag::BITS_ALLOCATED, bits),
        (Tag::BITS_STORED, bits),
        (Tag::HIGH_BIT, bits - 1),
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

fn encode(options: &HashMap<String, String>) -> Result<(), String> {
    let raw = std::fs::read(required(options, "raw")?).map_err(|e| e.to_string())?;
    let reversible = options.contains_key("reversible");
    let ht = options.contains_key("ht");
    let rpcl = options.contains_key("rpcl");
    let mut source = base_dataset(options, false)?;
    PixelData::create_new(&mut source)
        .map_err(|e| e.to_string())?
        .add_frame(&raw)
        .map_err(|e| e.to_string())?;
    let codec: Box<dyn Codec> = if ht {
        if reversible {
            if rpcl {
                Box::new(HtJpeg2000LosslessRpclCodec)
            } else {
                Box::new(HtJpeg2000LosslessCodec)
            }
        } else {
            Box::new(HtJpeg2000LossyCodec)
        }
    } else if reversible {
        Box::new(Jpeg2000LosslessCodec)
    } else {
        Box::new(Jpeg2000LossyCodec)
    };
    let params = if ht {
        match codec.default_params() {
            CodecParams::HtJpeg2000(mut p) => {
                if let Some(quality) = options.get("quality") {
                    p.quality = quality.parse().map_err(|_| "invalid quality".to_string())?;
                }
                p.insert_tlm_markers = options.contains_key("tlm");
                if rpcl {
                    p.progression_order = ProgressionOrder::Rpcl;
                }
                CodecParams::HtJpeg2000(p)
            }
            other => other,
        }
    } else {
        codec.default_params()
    };
    let mut encoded = source.clone();
    encoded.set_transfer_syntax(std::sync::Arc::new(codec.transfer_syntax().clone()));
    encoded.remove(Tag::PIXEL_DATA);
    let src = PixelDataRef::from_dataset(&source).map_err(|e| e.to_string())?;
    let mut dst = PixelData::create_new(&mut encoded).map_err(|e| e.to_string())?;
    codec
        .encode(&src, &mut dst, &params)
        .map_err(|e| e.to_string())?;
    let codestream = PixelDataRef::from_dataset(&encoded)
        .map_err(|e| e.to_string())?
        .get_frame(0)
        .map_err(|e| e.to_string())?
        .data()
        .map_err(|e| e.to_string())?
        .to_vec();
    std::fs::write(required(options, "out")?, codestream).map_err(|e| e.to_string())
}

fn decode(options: &HashMap<String, String>) -> Result<(), String> {
    let codestream = std::fs::read(required(options, "in")?).map_err(|e| e.to_string())?;
    let uid = required(options, "syntax")?;
    let codec: Box<dyn Codec> = if uid == syntaxes::JPEG_2000_LOSSLESS.uid() {
        Box::new(Jpeg2000LosslessCodec)
    } else if uid == syntaxes::JPEG_2000_LOSSY.uid() {
        Box::new(Jpeg2000LossyCodec)
    } else if uid == syntaxes::HTJ2K_LOSSLESS.uid() {
        Box::new(HtJpeg2000LosslessCodec)
    } else if uid == syntaxes::HTJ2K_LOSSLESS_RPCL.uid() {
        Box::new(HtJpeg2000LosslessRpclCodec)
    } else if uid == syntaxes::HTJ2K_LOSSY.uid() {
        Box::new(HtJpeg2000LossyCodec)
    } else {
        return Err(format!("unsupported syntax '{uid}'"));
    };
    let mut source = base_dataset(options, true)?;
    source.set_transfer_syntax(Arc::new(codec.transfer_syntax().clone()));
    {
        let fragments = source.insert_fragments(Tag::PIXEL_DATA, Vr::Ob);
        fragments.push_fragment_raw(Arc::new(MemoryBuffer::from(codestream)));
        fragments.set_offset_table(vec![0]);
    }
    let src = PixelDataRef::from_dataset(&source).map_err(|e| e.to_string())?;
    let mut destination = source.clone();
    destination.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    destination.remove(Tag::PIXEL_DATA);
    let mut dst = PixelData::create_new(&mut destination).map_err(|e| e.to_string())?;
    codec
        .decode(&src, &mut dst, &codec.default_params())
        .map_err(|e| e.to_string())?;
    let raw = PixelDataRef::from_dataset(&destination)
        .map_err(|e| e.to_string())?
        .get_frame(0)
        .map_err(|e| e.to_string())?
        .data()
        .map_err(|e| e.to_string())?
        .to_vec();
    std::fs::write(required(options, "out")?, raw).map_err(|e| e.to_string())
}
