#![allow(clippy::chunks_exact_to_as_chunks)]

use super::color::{rgb_to_ycc, JpegColorSpace};
use super::lossless::{LosslessFrame, Selection};
use super::sequential::{fancy_upsample, FrameExpectation, SequentialFrame};
use radicom::error::{CodecError, Error};
use radicom::model::tag::Tag;
use radicom::model::vr::Vr;
use radicom::pixel::photometric::PhotometricInterpretation;
use radicom::pixel::{PixelData, PixelDataRef};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Layout {
    width: usize,
    height: usize,
    bits_allocated: usize,
    bits_stored: u8,
    components: u8,
    planar: u16,
    photometric: PhotometricInterpretation,
    frame_length: usize,
}

impl Layout {
    fn from_source(data: &PixelDataRef<'_>, allow_palette: bool) -> Result<Self, String> {
        Self::from_values(
            data.width(),
            data.height(),
            data.bits_allocated().map_err(|error| error.to_string())?,
            data.bits_stored(),
            data.samples_per_pixel(),
            data.planar_configuration(),
            data.pixel_representation(),
            data.photometric_interpretation(),
            allow_palette,
        )
    }

    fn from_destination(data: &PixelData<'_>, allow_palette: bool) -> Result<Self, String> {
        Self::from_values(
            data.width(),
            data.height(),
            data.bits_allocated().map_err(|error| error.to_string())?,
            data.bits_stored(),
            data.samples_per_pixel(),
            data.planar_configuration(),
            data.pixel_representation(),
            data.photometric_interpretation(),
            allow_palette,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_values(
        width: u32,
        height: u32,
        bits_allocated: u16,
        bits_stored: u16,
        components: u16,
        planar: u16,
        pixel_representation: u16,
        photometric: Option<PhotometricInterpretation>,
        allow_palette: bool,
    ) -> Result<Self, String> {
        let width =
            usize::try_from(width).map_err(|_| "JPEG width does not fit usize".to_owned())?;
        let height =
            usize::try_from(height).map_err(|_| "JPEG height does not fit usize".to_owned())?;
        if width == 0 || height == 0 {
            return Err("JPEG dimensions must be non-zero".to_owned());
        }
        let bits_allocated = match bits_allocated {
            8 | 16 => usize::from(bits_allocated / 8),
            other => return Err(format!("JPEG BitsAllocated {other} is unsupported")),
        };
        let bits_stored =
            u8::try_from(bits_stored).map_err(|_| "JPEG BitsStored does not fit u8".to_owned())?;
        if !(2..=16).contains(&bits_stored) || usize::from(bits_stored) > bits_allocated * 8 {
            return Err(format!("JPEG BitsStored {bits_stored} is invalid"));
        }
        if pixel_representation > 1 {
            return Err(format!(
                "JPEG PixelRepresentation {pixel_representation} must be 0 or 1"
            ));
        }
        if pixel_representation == 1 && usize::from(bits_stored) != bits_allocated * 8 {
            return Err("JPEG signed samples require BitsStored equal to BitsAllocated".to_owned());
        }
        let components = u8::try_from(components)
            .map_err(|_| "JPEG SamplesPerPixel does not fit u8".to_owned())?;
        if !(1..=3).contains(&components) {
            return Err(format!("JPEG SamplesPerPixel {components} is unsupported"));
        }
        if components > 1 && planar > 1 {
            return Err(format!("JPEG PlanarConfiguration {planar} must be 0 or 1"));
        }
        let photometric =
            photometric.ok_or_else(|| "JPEG photometric interpretation is missing".to_owned())?;
        let expected_photometric = if components == 1 {
            matches!(
                Some(photometric),
                Some(PhotometricInterpretation::Monochrome1)
                    | Some(PhotometricInterpretation::Monochrome2)
            ) || (allow_palette && photometric == PhotometricInterpretation::PaletteColor)
        } else {
            matches!(
                Some(photometric),
                Some(PhotometricInterpretation::Rgb)
                    | Some(PhotometricInterpretation::YbrFull)
                    | Some(PhotometricInterpretation::YbrFull422)
            )
        };
        if !expected_photometric {
            return Err("JPEG photometric interpretation is unsupported".to_owned());
        }
        let frame_length = if photometric == PhotometricInterpretation::YbrFull422 {
            if components != 3 || planar != 0 || bits_allocated != 1 {
                return Err("YBR_FULL_422 requires interleaved 8-bit samples".to_owned());
            }
            width
                .div_ceil(2)
                .checked_mul(2)
                .and_then(|even_width| even_width.checked_mul(height))
                .and_then(|pixels| pixels.checked_mul(2))
                .ok_or_else(|| "JPEG YBR_FULL_422 frame size overflows usize".to_owned())?
        } else {
            width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(usize::from(components)))
                .and_then(|samples| samples.checked_mul(bits_allocated))
                .ok_or_else(|| "JPEG frame size overflows usize".to_owned())?
        };
        Ok(Self {
            width,
            height,
            bits_allocated,
            bits_stored,
            components,
            planar,
            photometric,
            frame_length,
        })
    }

    fn read_samples(&self, bytes: &[u8]) -> Result<Vec<u16>, String> {
        if bytes.len() < self.frame_length {
            return Err(format!(
                "native JPEG frame length {} is shorter than expected {}",
                bytes.len(),
                self.frame_length
            ));
        }
        if self.photometric == PhotometricInterpretation::YbrFull422 {
            return self.read_ybr422(bytes);
        }
        let sample_count = self
            .width
            .checked_mul(self.height)
            .and_then(|pixels| pixels.checked_mul(usize::from(self.components)))
            .ok_or_else(|| "JPEG sample count overflows usize".to_owned())?;
        let mut samples = Vec::with_capacity(sample_count);
        let mask = if self.bits_stored == 16 {
            u16::MAX
        } else {
            (1u16 << self.bits_stored) - 1
        };
        for sample in 0..sample_count {
            let source_index = self.native_offset(sample)?;
            let value = if self.bits_allocated == 1 {
                u16::from(bytes[source_index])
            } else {
                u16::from_le_bytes([bytes[source_index], bytes[source_index + 1]])
            };
            samples.push(value & mask);
        }
        Ok(samples)
    }

    fn write_samples(&self, samples: &[u16]) -> Result<Vec<u8>, String> {
        if self.photometric == PhotometricInterpretation::YbrFull422 {
            return self.write_ybr422(samples);
        }
        let expected = self
            .width
            .checked_mul(self.height)
            .and_then(|pixels| pixels.checked_mul(usize::from(self.components)))
            .ok_or_else(|| "JPEG sample count overflows usize".to_owned())?;
        if samples.len() != expected {
            return Err(format!(
                "decoded JPEG sample count {} does not match expected {expected}",
                samples.len()
            ));
        }
        let mut bytes = vec![0u8; self.frame_length];
        for (sample, &value) in samples.iter().enumerate() {
            let offset = self.native_offset(sample)?;
            if self.bits_allocated == 1 {
                bytes[offset] = value as u8;
            } else {
                bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        Ok(bytes)
    }

    fn native_offset(&self, sample: usize) -> Result<usize, String> {
        let pixel = sample / usize::from(self.components);
        let component = sample % usize::from(self.components);
        let pixels = self.width * self.height;
        let logical_sample = if self.planar == 1 {
            component * pixels + pixel
        } else {
            sample
        };
        logical_sample
            .checked_mul(self.bits_allocated)
            .ok_or_else(|| "JPEG native sample offset overflows usize".to_owned())
    }

    fn read_ybr422(&self, bytes: &[u8]) -> Result<Vec<u16>, String> {
        if bytes.len() < self.frame_length {
            return Err("native YBR_FULL_422 frame is truncated".to_owned());
        }
        let mut samples = Vec::with_capacity(self.width * self.height * 3);
        let row_bytes = self.width.div_ceil(2) * 4;
        for y in 0..self.height {
            for pair in 0..self.width.div_ceil(2) {
                let offset = y * row_bytes + pair * 4;
                let y0 = bytes[offset];
                let y1 = bytes[offset + 1];
                let cb = bytes[offset + 2];
                let cr = bytes[offset + 3];
                samples.extend([u16::from(y0), u16::from(cb), u16::from(cr)]);
                if pair * 2 + 1 < self.width {
                    samples.extend([u16::from(y1), u16::from(cb), u16::from(cr)]);
                }
            }
        }
        Ok(samples)
    }

    fn write_ybr422(&self, samples: &[u16]) -> Result<Vec<u8>, String> {
        let expected = self.width * self.height * 3;
        if samples.len() != expected {
            return Err(format!(
                "decoded JPEG sample count {} does not match expected {expected}",
                samples.len()
            ));
        }
        let mut bytes = vec![0u8; self.frame_length];
        let row_bytes = self.width.div_ceil(2) * 4;
        for y in 0..self.height {
            for pair in 0..self.width.div_ceil(2) {
                let x = pair * 2;
                let first = (y * self.width + x) * 3;
                let second = (y * self.width + (x + 1).min(self.width - 1)) * 3;
                let offset = y * row_bytes + pair * 4;
                bytes[offset] = samples[first].min(255) as u8;
                bytes[offset + 1] = samples[second].min(255) as u8;
                bytes[offset + 2] = (u32::from(samples[first + 1]) + u32::from(samples[second + 1]))
                    .div_ceil(2)
                    .min(255) as u8;
                bytes[offset + 3] = (u32::from(samples[first + 2]) + u32::from(samples[second + 2]))
                    .div_ceil(2)
                    .min(255) as u8;
            }
        }
        Ok(bytes)
    }
}

fn ybr_to_rgb(samples: &mut [u16], precision: u8) {
    let max = f64::from((1u32 << precision) - 1);
    let center = max / 2.0;
    for pixel in samples.chunks_exact_mut(3) {
        let y = f64::from(pixel[0]);
        let cb = f64::from(pixel[1]) - center;
        let cr = f64::from(pixel[2]) - center;
        pixel[0] = (y + 1.402 * cr).round().clamp(0.0, max) as u16;
        pixel[1] = (y - 0.344136 * cb - 0.714136 * cr).round().clamp(0.0, max) as u16;
        pixel[2] = (y + 1.772 * cb).round().clamp(0.0, max) as u16;
    }
}

pub(crate) fn encode_sequential(
    src: &PixelDataRef<'_>,
    dst: &mut PixelData<'_>,
    params: &radicom::pixel::codec::JpegParams,
    codec: &str,
    baseline: bool,
) -> Result<(), Error> {
    if src.transfer_syntax().is_encapsulated() || !dst.transfer_syntax().is_encapsulated() {
        return Err(operation_error(
            codec,
            "encode requires native source and encapsulated destination",
        ));
    }
    if params.quality > 100 || params.smoothing_factor > 100 {
        return Err(operation_error(
            codec,
            "quality and smoothing_factor must be 0..=100",
        ));
    }
    // design §9.3: parameters the sequential process cannot use are rejected
    // rather than silently ignored. Only `quality` and `smoothing_factor` are
    // meaningful here; `predictor`/`point_transform` belong to Process 14 and
    // `convert_colorspace_to_rgb` is a decode-only field.
    for (invalid, field, value) in [
        (params.predictor != 1, "predictor", params.predictor),
        (
            params.point_transform != 0,
            "point_transform",
            params.point_transform,
        ),
    ] {
        if invalid {
            return Err(operation_error(
                codec,
                &format!("sequential JPEG does not support {field} = {value}"),
            ));
        }
    }
    if params.convert_colorspace_to_rgb {
        return Err(operation_error(
            codec,
            "convert_colorspace_to_rgb is a decode-only parameter for sequential JPEG",
        ));
    }
    let layout = Layout::from_source(src, true).map_err(|m| operation_error(codec, &m))?;
    // Signed samples are coded as their two's-complement bit patterns, which is
    // what libjpeg sees too; the level shift cancels on decode. Only the bit
    // depth restricts the process.
    if (baseline && layout.bits_stored != 8) || (!baseline && !matches!(layout.bits_stored, 8 | 12))
    {
        return Err(operation_error(
            codec,
            "sequential JPEG supports 8-bit Process 1 or 8/12-bit Process 2/4 samples",
        ));
    }
    let sampling_half = matches!(
        params.sample_factor,
        radicom::pixel::codec::SampleFactor::Half
    );
    if sampling_half && layout.components != 3 {
        return Err(operation_error(
            codec,
            "4:2:2 sampling requires three components",
        ));
    }
    if layout.components == 3
        && !matches!(
            layout.photometric,
            PhotometricInterpretation::Rgb
                | PhotometricInterpretation::YbrFull
                | PhotometricInterpretation::YbrFull422
        )
    {
        return Err(operation_error(
            codec,
            "unsupported sequential JPEG photometric interpretation",
        ));
    }
    let mut frames = Vec::new();
    // The lossy encoder always writes YCbCr for a three-component frame:
    // libjpeg's jpeg_set_defaults converts RGB input to YCbCr, and
    // YBR_FULL/YBR_FULL_422 input is passed through as YCbCr.
    let colorspace = if layout.components == 3 {
        JpegColorSpace::Ycbcr
    } else {
        JpegColorSpace::from_photometric(layout.photometric)
    };
    for i in 0..src.number_of_frames() {
        let frame = src
            .get_frame(i)
            .map_err(|e| operation_error(codec, &format!("frame {i}: {e}")))?;
        let bytes = frame
            .data()
            .map_err(|e| operation_error(codec, &format!("frame {i}: {e}")))?;
        let mut samples = layout
            .read_samples(bytes.as_ref())
            .map_err(|m| operation_error(codec, &format!("frame {i}: {m}")))?;
        if layout.photometric == PhotometricInterpretation::Rgb {
            rgb_to_ycc(&mut samples, layout.bits_stored);
        }
        let frame = SequentialFrame::new(
            layout.width,
            layout.height,
            layout.bits_stored,
            layout.components,
            samples,
        )
        .map_err(|m| operation_error(codec, &format!("frame {i}: {m}")))?;
        frames.push(
            frame
                .encode_with_colorspace(
                    params.quality,
                    !baseline,
                    sampling_half,
                    params.smoothing_factor,
                    (params.restart_interval != 0).then_some(params.restart_interval),
                    colorspace,
                )
                .map_err(|m| operation_error(codec, &format!("frame {i}: {m}")))?,
        );
    }
    for frame in frames {
        dst.add_frame(frame)
            .map_err(|e| operation_error(codec, &e.to_string()))?;
    }
    if layout.components == 3 {
        let pi = if sampling_half {
            "YBR_FULL_422"
        } else {
            "YBR_FULL"
        };
        dst.dataset_mut()
            .insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, pi);
        dst.dataset_mut()
            .insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
    }
    Ok(())
}

pub(crate) fn decode_sequential(
    src: &PixelDataRef<'_>,
    dst: &mut PixelData<'_>,
    params: &radicom::pixel::codec::JpegParams,
    codec: &str,
    baseline: bool,
) -> Result<(), Error> {
    if !src.transfer_syntax().is_encapsulated() || dst.transfer_syntax().is_encapsulated() {
        return Err(operation_error(
            codec,
            "decode requires encapsulated source and native destination",
        ));
    }
    let expected = FrameExpectation {
        width: usize::try_from(dst.width()).unwrap_or(0),
        height: usize::try_from(dst.height()).unwrap_or(0),
        components: u8::try_from(dst.samples_per_pixel()).unwrap_or(0),
        precision: u8::try_from(dst.bits_stored()).unwrap_or(0),
    };
    let mut frames = Vec::new();
    for i in 0..src.number_of_frames() {
        let frame_bytes = src
            .get_frame(i)
            .map_err(|e| operation_error(codec, &format!("frame {i}: {e}")))?;
        let bytes = frame_bytes
            .data()
            .map_err(|e| operation_error(codec, &format!("frame {i}: {e}")))?;
        let frame = SequentialFrame::decode_expected(bytes.as_ref(), Some(&expected), baseline)
            .map_err(|m| operation_error(codec, &format!("frame {i}: {m}")))?;
        if (baseline && frame.precision != 8) || (!baseline && !matches!(frame.precision, 8 | 12)) {
            return Err(operation_error(
                codec,
                &format!("frame {i}: codestream layout does not match destination"),
            ));
        }
        frames.push(frame);
    }
    let photometric = src.photometric_interpretation();
    let convert = params.convert_colorspace_to_rgb
        && matches!(
            photometric,
            Some(PhotometricInterpretation::YbrFull) | Some(PhotometricInterpretation::YbrFull422)
        )
        && frames.first().is_some_and(|frame| frame.components == 3);
    if convert && dst.pixel_representation() != 0 {
        return Err(operation_error(
            codec,
            "color conversion is unsupported for signed samples",
        ));
    }
    let mut layout = Layout::from_destination(dst, true).map_err(|m| operation_error(codec, &m))?;
    if convert
        && (layout.photometric == PhotometricInterpretation::YbrFull422 || layout.planar == 1)
    {
        // Conversion emits three interleaved samples per pixel labelled RGB. A
        // 4:2:2 destination packs two bytes per pixel and a planar destination
        // writes per-plane; both must be rebuilt as interleaved RGB so the
        // bytes match the metadata we set below.
        layout = Layout::from_values(
            dst.width(),
            dst.height(),
            dst.bits_allocated()
                .map_err(|error| operation_error(codec, &error.to_string()))?,
            dst.bits_stored(),
            3,
            0,
            dst.pixel_representation(),
            Some(PhotometricInterpretation::Rgb),
            false,
        )
        .map_err(|m| operation_error(codec, &m))?;
    }
    let mut output = Vec::with_capacity(frames.len());
    for mut frame in frames {
        if convert {
            fancy_upsample(
                &mut frame.samples,
                frame.width,
                frame.height,
                frame.precision,
                frame.components,
                &frame.sampling,
            );
            ybr_to_rgb(&mut frame.samples, frame.precision);
        }
        output.push(
            layout
                .write_samples(&frame.samples)
                .map_err(|m| operation_error(codec, &m))?,
        );
    }
    if convert {
        // Update the destination contract before appending frames:
        // `PixelData::add_frame` derives each frame's natural offset from the
        // current metadata, so a YBR_FULL_422 destination would truncate the
        // second and later 3-byte-per-pixel RGB frames.
        dst.dataset_mut()
            .insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "RGB");
        dst.dataset_mut()
            .insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
    }
    for frame in output {
        dst.add_frame(frame)
            .map_err(|e| operation_error(codec, &e.to_string()))?;
    }
    Ok(())
}

pub(crate) fn encode_lossless(
    src: &PixelDataRef<'_>,
    dst: &mut PixelData<'_>,
    params: &radicom::pixel::codec::JpegParams,
    predictor: u8,
    codec: &str,
) -> Result<(), Error> {
    if src.transfer_syntax().is_encapsulated() || !dst.transfer_syntax().is_encapsulated() {
        return Err(operation_error(
            codec,
            "encode requires native source and encapsulated destination",
        ));
    }
    if params.convert_colorspace_to_rgb {
        return Err(operation_error(
            codec,
            "lossless encode does not perform color conversion",
        ));
    }
    let layout =
        Layout::from_source(src, true).map_err(|message| operation_error(codec, &message))?;
    let point_transform = u8::try_from(params.point_transform)
        .map_err(|_| operation_error(codec, "point transform must fit in u8"))?;
    if point_transform >= layout.bits_stored {
        return Err(operation_error(
            codec,
            "point transform must be less than BitsStored",
        ));
    }
    let mut encoded_frames = Vec::new();
    let colorspace = JpegColorSpace::from_photometric(layout.photometric);
    for frame_index in 0..src.number_of_frames() {
        let buffer = src
            .get_frame(frame_index)
            .map_err(|error| operation_error(codec, &format!("frame {frame_index}: {error}")))?;
        let bytes = buffer
            .data()
            .map_err(|error| operation_error(codec, &format!("frame {frame_index}: {error}")))?;
        let samples = layout.read_samples(bytes.as_ref()).map_err(|message| {
            operation_error(codec, &format!("frame {frame_index}: {message}"))
        })?;
        let frame = LosslessFrame::new(
            layout.width,
            layout.height,
            layout.bits_stored,
            layout.components,
            samples,
        )
        .map_err(|message| operation_error(codec, &format!("frame {frame_index}: {message}")))?;
        encoded_frames.push(
            frame
                .encode_with_colorspace(
                    predictor,
                    point_transform,
                    (params.restart_interval != 0).then_some(params.restart_interval),
                    colorspace,
                )
                .map_err(|message| {
                    operation_error(codec, &format!("frame {frame_index}: {message}"))
                })?,
        );
    }
    for frame in encoded_frames {
        dst.add_frame(frame)
            .map_err(|error| operation_error(codec, &error.to_string()))?;
    }
    Ok(())
}

pub(crate) fn decode_lossless(
    src: &PixelDataRef<'_>,
    dst: &mut PixelData<'_>,
    params: &radicom::pixel::codec::JpegParams,
    selection: Selection,
    codec: &str,
) -> Result<(), Error> {
    if !src.transfer_syntax().is_encapsulated() || dst.transfer_syntax().is_encapsulated() {
        return Err(operation_error(
            codec,
            "decode requires encapsulated source and native destination",
        ));
    }
    if params.convert_colorspace_to_rgb {
        return Err(operation_error(
            codec,
            "lossless color conversion is not implemented",
        ));
    }
    let layout =
        Layout::from_destination(dst, true).map_err(|message| operation_error(codec, &message))?;
    let expected = FrameExpectation {
        width: layout.width,
        height: layout.height,
        components: layout.components,
        precision: layout.bits_stored,
    };
    let mut decoded_frames = Vec::new();
    for frame_index in 0..src.number_of_frames() {
        let buffer = src
            .get_frame(frame_index)
            .map_err(|error| operation_error(codec, &format!("frame {frame_index}: {error}")))?;
        let bytes = buffer
            .data()
            .map_err(|error| operation_error(codec, &format!("frame {frame_index}: {error}")))?;
        let frame = LosslessFrame::decode(bytes.as_ref(), selection, Some(&expected)).map_err(
            |message| operation_error(codec, &format!("frame {frame_index}: {message}")),
        )?;
        if frame.width != layout.width
            || frame.height != layout.height
            || frame.precision != layout.bits_stored
            || frame.components != layout.components
        {
            return Err(operation_error(
                codec,
                &format!("frame {frame_index}: codestream layout does not match destination"),
            ));
        }
        decoded_frames.push(layout.write_samples(&frame.samples).map_err(|message| {
            operation_error(codec, &format!("frame {frame_index}: {message}"))
        })?);
    }
    for frame in decoded_frames {
        dst.add_frame(frame)
            .map_err(|error| operation_error(codec, &error.to_string()))?;
    }
    Ok(())
}

fn operation_error(codec: &str, message: &str) -> Error {
    CodecError::Operation {
        codec: codec.to_owned(),
        message: message.to_owned(),
    }
    .into()
}
