// U-Shoot Raw Decoder — sidecar binary.
//
// Invoked by the main U-Shoot Tauri app over CLI:
//   ushoot-raw-decoder decode     --input <raw-path> --output <binary-path>
//   ushoot-raw-decoder dimensions --input <raw-path>
//   ushoot-raw-decoder exif       --input <raw-path>
//
// `decode` writes a packed binary blob to <output> (header + RGB8
// sRGB pixels). `dimensions` and `exif` write JSON to stdout.
//
// This binary is LGPL-2.1 (because it links rawler). Its full
// source is published with U-Shoot, which satisfies the LGPL §6
// requirement: recipients can rebuild it with a modified rawler.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Magic bytes at the start of the binary blob written by `decode`.
/// Lets the main app detect a corrupted / wrong file.
const MAGIC: &[u8; 4] = b"USRD";
/// Binary format version. Bump when the layout changes.
const FORMAT_VERSION: u8 = 1;

#[derive(Parser)]
#[command(
    name = "ushoot-raw-decoder",
    version,
    about = "RAW decoder sidecar for U-Shoot Desktop (LGPL-2.1)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decode a RAW file to RGB8 sRGB pixels, written to a binary file.
    Decode {
        /// Path to the RAW input file (CR3, NEF, ARW, etc.).
        #[arg(long)]
        input: PathBuf,
        /// Path where the decoded blob (header + RGB8 pixels) will
        /// be written.
        #[arg(long)]
        output: PathBuf,
    },
    /// Print `{"width": N, "height": M}` to stdout.
    Dimensions {
        #[arg(long)]
        input: PathBuf,
    },
    /// Print a JSON object with RAW-level EXIF + orientation + wb to
    /// stdout.
    Exif {
        #[arg(long)]
        input: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Decode { input, output } => decode(&input, &output),
        Command::Dimensions { input } => dimensions(&input),
        Command::Exif { input } => exif(&input),
    }
}

// ---------- decode ----------

fn decode(input: &Path, output: &Path) -> Result<()> {
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
    use rawler::rawsource::RawSource;

    let file_bytes = std::fs::read(input)
        .with_context(|| format!("read RAW file {}", input.display()))?;

    let source = RawSource::new_from_slice(&file_bytes);
    let decoder = rawler::get_decoder(&source)
        .map_err(|e| anyhow!("get_decoder: {:?}", e))?;

    let mut raw_image = decoder
        .raw_image(&source, &RawDecodeParams::default(), false)
        .map_err(|e| anyhow!("raw_image decode: {:?}", e))?;

    // Save original levels BEFORE mutation, then push whitelevel to
    // u32::MAX so RawDevelop preserves highlights. Same trick as the
    // pre-sidecar in-tree implementation.
    let original_white_level = raw_image
        .whitelevel
        .0
        .first()
        .cloned()
        .unwrap_or(u16::MAX as u32) as f32;
    let original_black_level = raw_image
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);

    let headroom_white_level = u32::MAX as f32;
    for level in raw_image.whitelevel.0.iter_mut() {
        *level = u32::MAX;
    }

    // Develop WITHOUT the SRgb step — we'll apply our own gamma curve
    // after rescale, matching the in-tree pipeline byte-for-byte.
    let mut develop = RawDevelop::default();
    develop.steps.retain(|&s| s != ProcessingStep::SRgb);

    let developed = develop
        .develop_intermediate(&raw_image)
        .map_err(|e| anyhow!("develop_intermediate: {:?}", e))?;

    let dim = developed.dim();
    let width = dim.w as u32;
    let height = dim.h as u32;

    let denom = (original_white_level - original_black_level).max(1.0);
    let rescale_factor = (headroom_white_level - original_black_level) / denom;
    let safe_highlight_compression = 4.0_f32.max(1.01);

    let rgb8: Vec<u8> = match developed {
        Intermediate::ThreeColor(mut pixels) => {
            for p in pixels.data.iter_mut() {
                let r = (p[0] * rescale_factor).max(0.0);
                let g = (p[1] * rescale_factor).max(0.0);
                let b = (p[2] * rescale_factor).max(0.0);
                let max_c = r.max(g).max(b);

                let (fr, fg, fb) = if max_c > 1.0 {
                    let min_c = r.min(g).min(b);
                    let cf = (1.0 - (max_c - 1.0) / (safe_highlight_compression - 1.0))
                        .clamp(0.0, 1.0);
                    let cr = min_c + (r - min_c) * cf;
                    let cg = min_c + (g - min_c) * cf;
                    let cb = min_c + (b - min_c) * cf;
                    let cmax = cr.max(cg).max(cb);
                    if cmax > 1e-6 {
                        let rs = max_c / cmax;
                        (cr * rs, cg * rs, cb * rs)
                    } else {
                        (max_c, max_c, max_c)
                    }
                } else {
                    (r, g, b)
                };

                p[0] = fr;
                p[1] = fg;
                p[2] = fb;
            }

            let mut out = Vec::with_capacity((width as usize) * (height as usize) * 3);
            for p in pixels.data.iter() {
                let r = linear_to_srgb(p[0].clamp(0.0, 1.0));
                let g = linear_to_srgb(p[1].clamp(0.0, 1.0));
                let b = linear_to_srgb(p[2].clamp(0.0, 1.0));
                out.push((r * 255.0).clamp(0.0, 255.0) as u8);
                out.push((g * 255.0).clamp(0.0, 255.0) as u8);
                out.push((b * 255.0).clamp(0.0, 255.0) as u8);
            }
            out
        }
        Intermediate::Monochrome(mut pixels) => {
            for p in pixels.data.iter_mut() {
                *p *= rescale_factor;
            }
            let mut out = Vec::with_capacity((width as usize) * (height as usize) * 3);
            for p in pixels.data.iter() {
                let v = linear_to_srgb(p.clamp(0.0, 1.0));
                let b = (v * 255.0).clamp(0.0, 255.0) as u8;
                out.push(b);
                out.push(b);
                out.push(b);
            }
            out
        }
        _ => {
            return Err(anyhow!(
                "unsupported Intermediate variant (only ThreeColor and Monochrome are handled)"
            ));
        }
    };

    // Write packed binary blob: 16-byte header + raw RGB8 pixels.
    // Header layout:
    //   bytes 0..4   = MAGIC "USRD"
    //   byte  4      = FORMAT_VERSION (1)
    //   byte  5      = channels (always 3 for now)
    //   bytes 6..8   = reserved (zero)
    //   bytes 8..12  = width  (u32 little-endian)
    //   bytes 12..16 = height (u32 little-endian)
    let mut writer = BufWriter::new(
        File::create(output)
            .with_context(|| format!("create output blob {}", output.display()))?,
    );
    writer.write_all(MAGIC)?;
    writer.write_all(&[FORMAT_VERSION, 3, 0, 0])?;
    writer.write_all(&width.to_le_bytes())?;
    writer.write_all(&height.to_le_bytes())?;
    writer.write_all(&rgb8)?;
    writer.flush()?;

    Ok(())
}

#[inline(always)]
fn linear_to_srgb(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

// ---------- dimensions ----------

#[derive(Serialize)]
struct DimensionsOut {
    width: u32,
    height: u32,
}

fn dimensions(input: &Path) -> Result<()> {
    use rawler::decoders::RawDecodeParams;
    use rawler::rawsource::RawSource;

    let file_bytes = std::fs::read(input)
        .with_context(|| format!("read RAW file {}", input.display()))?;
    let source = RawSource::new_from_slice(&file_bytes);
    let decoder = rawler::get_decoder(&source)
        .map_err(|e| anyhow!("get_decoder: {:?}", e))?;
    let raw_image = decoder
        .raw_image(&source, &RawDecodeParams::default(), false)
        .map_err(|e| anyhow!("raw_image decode: {:?}", e))?;

    let out = DimensionsOut {
        width: raw_image.width as u32,
        height: raw_image.height as u32,
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

// ---------- exif ----------

#[derive(Serialize)]
struct ExifOut {
    width: u32,
    height: u32,
    make: String,
    model: String,
    /// PTP/EXIF orientation code already converted from the rawler
    /// enum: 1=Normal, 2=HorizontalFlip, 3=Rotate180, 4=VerticalFlip,
    /// 5=Transpose, 6=Rotate90, 7=Transverse, 8=Rotate270.
    /// Returns 1 (Normal) when rawler can't determine.
    orientation: u16,
    /// As-shot white balance coefficients: [R, G1, B, G2].
    /// Main app uses these to compute the colour temperature in K
    /// via the McCamy formula (kept out of the sidecar to avoid
    /// growing it unnecessarily).
    wb_coeffs: [f32; 4],
}

fn exif(input: &Path) -> Result<()> {
    let raw = rawler::decode_file(input)
        .map_err(|e| anyhow!("decode_file: {:?}", e))?;

    let orientation: u16 = match raw.orientation {
        rawler::Orientation::Normal => 1,
        rawler::Orientation::HorizontalFlip => 2,
        rawler::Orientation::Rotate180 => 3,
        rawler::Orientation::VerticalFlip => 4,
        rawler::Orientation::Transpose => 5,
        rawler::Orientation::Rotate90 => 6,
        rawler::Orientation::Transverse => 7,
        rawler::Orientation::Rotate270 => 8,
        rawler::Orientation::Unknown => 1,
    };

    let out = ExifOut {
        width: raw.width as u32,
        height: raw.height as u32,
        make: raw.clean_make.clone(),
        model: raw.clean_model.clone(),
        orientation,
        wb_coeffs: raw.wb_coeffs,
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
