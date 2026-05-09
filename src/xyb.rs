//! XYB color space transform and ICC profile generation.
//!
//! Implements the XYB color space used by JPEG XL, ported from jpegli/libjxl.
//! When the `--xyb` option is used, image pixel data is converted from sRGB to
//! the scaled XYB representation, and an ICC profile is embedded so that
//! ICC-aware software can convert back to a displayable color space.

use crate::headers::{Chunk, make_iccp};
use crate::png::{PngData, PngImage};
use crate::{Deflater, PngError, PngResult};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Opsin / XYB constants (from jpegli lib/cms/opsin_params.h)
// ---------------------------------------------------------------------------

const K_M00: f64 = 0.30;
const K_M01: f64 = 1.0 - 0.078 - 0.30; // 0.622
const K_M02: f64 = 0.078;

const K_M10: f64 = 0.23;
const K_M11: f64 = 1.0 - 0.078 - 0.23; // 0.692
const K_M12: f64 = 0.078;

const K_M20: f64 = 0.243_422_689_245_478_19;
const K_M21: f64 = 0.204_767_444_244_968_21;
const K_M22: f64 = 1.0 - K_M20 - K_M21;

const OPSIN_ABSORBANCE_MATRIX: [[f64; 3]; 3] = [
    [K_M00, K_M01, K_M02],
    [K_M10, K_M11, K_M12],
    [K_M20, K_M21, K_M22],
];

const OPSIN_ABSORBANCE_BIAS: f64 = 0.003_793_073_255_275_449_3;

const NEG_OPSIN_ABSORBANCE_BIAS_RGB: [f64; 3] = [
    -OPSIN_ABSORBANCE_BIAS,
    -OPSIN_ABSORBANCE_BIAS,
    -OPSIN_ABSORBANCE_BIAS,
];

const SCALED_XYB_OFFSET: [f64; 3] = [0.015_386_134, 0.0, 0.277_704_59];
const SCALED_XYB_SCALE: [f64; 3] = [22.995_788_804, 1.183_000_077, 1.502_141_333];

// Derived constants (matching jpegli opsin_params.h)
const XYB_OFFSET: [f64; 3] = [
    SCALED_XYB_OFFSET[0] + SCALED_XYB_OFFSET[1],
    SCALED_XYB_OFFSET[1] - SCALED_XYB_OFFSET[0] + (1.0 / SCALED_XYB_SCALE[0]),
    SCALED_XYB_OFFSET[1] + SCALED_XYB_OFFSET[2],
];

const fn reciprocal_sum(r1: f64, r2: f64) -> f64 {
    (r1 * r2) / (r1 + r2)
}

const XYB_SCALE: [f64; 3] = [
    reciprocal_sum(SCALED_XYB_SCALE[0], SCALED_XYB_SCALE[1]),
    reciprocal_sum(SCALED_XYB_SCALE[0], SCALED_XYB_SCALE[1]),
    reciprocal_sum(SCALED_XYB_SCALE[1], SCALED_XYB_SCALE[2]),
];

// ---------------------------------------------------------------------------
// sRGB transfer function
// ---------------------------------------------------------------------------

/// sRGB EOTF: gamma-encoded [0,1] → linear [0,1]
fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

// ---------------------------------------------------------------------------
// Forward XYB transform
// ---------------------------------------------------------------------------

/// Convert a single linear-light RGB pixel to *scaled* XYB.
fn linear_rgb_to_scaled_xyb(r: f64, g: f64, b: f64) -> [f64; 3] {
    // Opsin absorbance
    let mut mixed = [
        OPSIN_ABSORBANCE_MATRIX[0][0] * r
            + OPSIN_ABSORBANCE_MATRIX[0][1] * g
            + OPSIN_ABSORBANCE_MATRIX[0][2] * b
            + OPSIN_ABSORBANCE_BIAS,
        OPSIN_ABSORBANCE_MATRIX[1][0] * r
            + OPSIN_ABSORBANCE_MATRIX[1][1] * g
            + OPSIN_ABSORBANCE_MATRIX[1][2] * b
            + OPSIN_ABSORBANCE_BIAS,
        OPSIN_ABSORBANCE_MATRIX[2][0] * r
            + OPSIN_ABSORBANCE_MATRIX[2][1] * g
            + OPSIN_ABSORBANCE_MATRIX[2][2] * b
            + OPSIN_ABSORBANCE_BIAS,
    ];

    // Clamp negatives (safety with wide-gamut)
    for v in &mut mixed {
        if *v < 0.0 {
            *v = 0.0;
        }
    }

    // Cube root + subtract cbrt(bias)
    let cbrt_bias = OPSIN_ABSORBANCE_BIAS.cbrt();
    mixed[0] = mixed[0].cbrt() - cbrt_bias;
    mixed[1] = mixed[1].cbrt() - cbrt_bias;
    mixed[2] = mixed[2].cbrt() - cbrt_bias;

    // XYB rotation: X = 0.5*(L-M), Y = 0.5*(L+M), B = S
    let x = 0.5 * (mixed[0] - mixed[1]);
    let y = 0.5 * (mixed[0] + mixed[1]);
    let b_val = mixed[2];

    // ScaleXYBRow (from jpegli): maps to [0,1] range
    // Note: B channel uses (B - Y + offset) * scale
    let scaled_b = (b_val - y + SCALED_XYB_OFFSET[2]) * SCALED_XYB_SCALE[2];
    let scaled_x = (x + SCALED_XYB_OFFSET[0]) * SCALED_XYB_SCALE[0];
    let scaled_y = (y + SCALED_XYB_OFFSET[1]) * SCALED_XYB_SCALE[1];

    [scaled_x, scaled_y, scaled_b]
}

// ---------------------------------------------------------------------------
// Image conversion
// ---------------------------------------------------------------------------

/// Convert an 8-bit sRGB PNG image to scaled XYB.
///
/// The image must be RGB, RGBA, Grayscale, or GrayscaleAlpha with 8-bit depth.
/// Alpha is dropped during conversion (XYB is 3-channel).
/// Returns a new `PngImage` with RGB color type and the pixel data in XYB.
pub fn convert_to_xyb(image: &PngImage) -> PngResult<PngImage> {
    use crate::colors::{BitDepth, ColorType};

    let channels = image.ihdr.color_type.channels_per_pixel() as usize;
    let is_gray = image.ihdr.color_type.is_gray();

    if image.ihdr.bit_depth != BitDepth::Eight {
        return Err(PngError::new(
            "XYB conversion currently only supports 8-bit images",
        ));
    }

    let width = image.ihdr.width as usize;
    let height = image.ihdr.height as usize;
    let pixels = width * height;

    // Output: 3 channels (RGB holding XYB), 8 bits
    let mut out = Vec::with_capacity(pixels * 3);

    for i in 0..pixels {
        let offset = i * channels;

        // Read source pixel
        let (sr, sg, sb) = if is_gray {
            let g = image.data[offset];
            (g, g, g)
        } else {
            (
                image.data[offset],
                image.data[offset + 1],
                image.data[offset + 2],
            )
        };

        // sRGB gamma decode → linear
        let lr = srgb_to_linear(sr as f64 / 255.0);
        let lg = srgb_to_linear(sg as f64 / 255.0);
        let lb = srgb_to_linear(sb as f64 / 255.0);

        // Linear RGB → scaled XYB
        let xyb = linear_rgb_to_scaled_xyb(lr, lg, lb);

        // Quantize to 8-bit [0, 255] with clamping
        for &v in &xyb {
            out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }

    Ok(PngImage {
        ihdr: crate::headers::IhdrData {
            width: image.ihdr.width,
            height: image.ihdr.height,
            color_type: ColorType::RGB {
                transparent_color: None,
            },
            bit_depth: BitDepth::Eight,
            interlaced: image.ihdr.interlaced,
        },
        data: out,
    })
}

// ---------------------------------------------------------------------------
// ICC profile generation (ported from jpegli CreateICCLutAtoBTagForXYB et al.)
// ---------------------------------------------------------------------------

/// Generate a complete ICC profile for the scaled XYB color space.
pub fn create_xyb_icc_profile() -> Vec<u8> {
    let mut header = Vec::new();
    let mut tagtable = Vec::new();
    let mut tags = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();

    // -- Header (128 bytes) --
    create_icc_header(&mut header);

    // -- Tag table (count placeholder) --
    push_u32(0, &mut tagtable);

    let mut tag_offset: usize = 0;
    let mut tag_size: usize = 0;

    // desc
    create_mluc_tag("RGB_D65_SRG_Rel_Lin XYB", &mut tags);
    finalize_tag(&mut tags, &mut tag_offset, &mut tag_size);
    add_to_tag_table(b"desc", tag_offset, tag_size, &mut tagtable, &mut offsets);

    // cprt
    create_mluc_tag("CC0", &mut tags);
    finalize_tag(&mut tags, &mut tag_offset, &mut tag_size);
    add_to_tag_table(b"cprt", tag_offset, tag_size, &mut tagtable, &mut offsets);

    // wtpt (D50)
    create_xyz_tag([0.964203, 1.0, 0.824905], &mut tags);
    finalize_tag(&mut tags, &mut tag_offset, &mut tag_size);
    add_to_tag_table(b"wtpt", tag_offset, tag_size, &mut tagtable, &mut offsets);

    // chad (chromatic adaptation matrix for sRGB D65 → D50)
    create_chad_tag(&mut tags);
    finalize_tag(&mut tags, &mut tag_offset, &mut tag_size);
    add_to_tag_table(b"chad", tag_offset, tag_size, &mut tagtable, &mut offsets);

    // A2B0 (XYB → XYZ)
    create_a2b0_tag_for_xyb(&mut tags);
    finalize_tag(&mut tags, &mut tag_offset, &mut tag_size);
    add_to_tag_table(b"A2B0", tag_offset, tag_size, &mut tagtable, &mut offsets);

    // B2A0 (no-op, required by some software)
    create_noop_b2a0_tag(&mut tags);
    finalize_tag(&mut tags, &mut tag_offset, &mut tag_size);
    add_to_tag_table(b"B2A0", tag_offset, tag_size, &mut tagtable, &mut offsets);

    // -- Fix up tag table: count and offsets --
    write_u32(offsets.len() as u32, 0, &mut tagtable);
    for (i, &off) in offsets.iter().enumerate() {
        let abs_offset = off + header.len() + tagtable.len();
        write_u32(abs_offset as u32, 4 + 12 * i + 4, &mut tagtable);
    }

    // -- Fix up total profile size --
    let total = header.len() + tagtable.len() + tags.len();
    write_u32(total as u32, 0, &mut header);

    // -- Assemble --
    let mut icc = header;
    icc.extend_from_slice(&tagtable);
    icc.extend_from_slice(&tags);

    // -- Compute and embed MD5 profile ID --
    let mut icc_for_md5 = icc.clone();
    if icc_for_md5.len() >= 68 {
        // Zero out profile flags (offset 44, 4 bytes) and rendering intent (offset 64, 4 bytes)
        icc_for_md5[44..48].fill(0);
        icc_for_md5[64..68].fill(0);
    }
    if icc_for_md5.len() >= 100 {
        // Zero out profile ID field itself (offset 84, 16 bytes)
        icc_for_md5[84..100].fill(0);
    }
    let digest = md5_digest(&icc_for_md5);
    icc[84..100].copy_from_slice(&digest);

    icc
}

/// Build an iCCP chunk containing the XYB ICC profile.
pub fn make_xyb_iccp_chunk() -> Chunk {
    let icc = create_xyb_icc_profile();
    // Use fast compression; oxipng will recompress during optimization.
    let deflater = Deflater::Libdeflater { compression: 1 };
    make_iccp(&icc, deflater, None).expect("failed to compress XYB ICC profile")
}

/// Apply XYB conversion to a `PngData` in-place.
///
/// This converts the pixel data from sRGB to XYB, replaces any existing
/// color-related chunks (sRGB, iCCP) with the XYB ICC profile, and updates
/// the raw image data.
///
/// Returns an error if the source image is not sRGB (e.g. has a non-sRGB ICC
/// profile or a cICP chunk indicating a different color space).
pub fn apply_xyb_conversion(png: &mut PngData) -> PngResult<()> {
    use crate::headers::{extract_icc, srgb_rendering_intent};

    // Reject images that are not in the sRGB color space.
    // The XYB transform hard-codes sRGB primaries and transfer function;
    // applying it to other color spaces would produce incorrect results.
    if let Some(iccp) = png.aux_chunks.iter().find(|c| &c.name == b"iCCP") {
        if let Some(icc_data) = extract_icc(iccp, None) {
            if srgb_rendering_intent(&icc_data).is_none() {
                return Err(PngError::new(
                    "XYB conversion requires an sRGB input; the image has a \
                     non-sRGB ICC profile. Convert to sRGB first.",
                ));
            }
        }
    }
    if png.aux_chunks.iter().any(|c| &c.name == b"cICP") {
        return Err(PngError::new(
            "XYB conversion requires an sRGB input; the image has a cICP \
             chunk indicating a non-sRGB color space.",
        ));
    }

    let xyb_image = convert_to_xyb(&png.raw)?;
    png.raw = Arc::new(xyb_image);

    // Remove any existing sRGB or iCCP chunks (they no longer apply)
    png.aux_chunks
        .retain(|c| &c.name != b"sRGB" && &c.name != b"iCCP");

    // Embed the XYB ICC profile
    png.aux_chunks.insert(0, make_xyb_iccp_chunk());

    // Recompress IDAT for the new pixel data
    let (filtered, _) = png
        .raw
        .filter_image(crate::filters::FilterStrategy::NONE, false);
    let deflater = Deflater::Libdeflater { compression: 1 };
    png.idat_data = deflater.deflate(&filtered, None)?;

    Ok(())
}

// ========================== ICC binary helpers ==============================

fn write_u32(value: u32, pos: usize, buf: &mut Vec<u8>) {
    if buf.len() < pos + 4 {
        buf.resize(pos + 4, 0);
    }
    buf[pos] = (value >> 24) as u8;
    buf[pos + 1] = (value >> 16) as u8;
    buf[pos + 2] = (value >> 8) as u8;
    buf[pos + 3] = value as u8;
}

fn write_u16(value: u16, pos: usize, buf: &mut Vec<u8>) {
    if buf.len() < pos + 2 {
        buf.resize(pos + 2, 0);
    }
    buf[pos] = (value >> 8) as u8;
    buf[pos + 1] = value as u8;
}

fn push_u32(value: u32, buf: &mut Vec<u8>) {
    let pos = buf.len();
    buf.resize(pos + 4, 0);
    write_u32(value, pos, buf);
}

fn push_u16(value: u16, buf: &mut Vec<u8>) {
    let pos = buf.len();
    buf.resize(pos + 2, 0);
    write_u16(value, pos, buf);
}

fn push_u8(value: u8, buf: &mut Vec<u8>) {
    buf.push(value);
}

fn push_tag(tag: &[u8; 4], buf: &mut Vec<u8>) {
    buf.extend_from_slice(tag);
}

fn push_s15fixed16(value: f64, buf: &mut Vec<u8>) {
    let i = (value * 65536.0).round() as i32;
    let u = i as u32;
    push_u32(u, buf);
}

// ========================== ICC structure builders ==========================

fn create_icc_header(header: &mut Vec<u8>) {
    header.resize(128, 0);

    write_u32(0, 0, header); // size placeholder
    header[4..8].copy_from_slice(b"jxl "); // CMM
    write_u32(0x04400000, 8, header); // version 4.4
    header[12..16].copy_from_slice(b"scnr"); // device class
    header[16..20].copy_from_slice(b"RGB "); // color space
    header[20..24].copy_from_slice(b"XYZ "); // PCS

    // Date/time: 2019-12-01 00:00:00 (matches jpegli)
    write_u16(2019, 24, header);
    write_u16(12, 26, header);
    write_u16(1, 28, header);
    write_u16(0, 30, header);
    write_u16(0, 32, header);
    write_u16(0, 34, header);

    header[36..40].copy_from_slice(b"acsp"); // signature
    header[40..44].copy_from_slice(b"APPL"); // primary platform
    write_u32(0, 44, header); // flags
    write_u32(0, 48, header); // device manufacturer
    write_u32(0, 52, header); // device model
    write_u32(0, 56, header); // device attributes
    write_u32(0, 60, header); // device attributes (cont)
    write_u32(0, 64, header); // rendering intent (perceptual)

    // D50 illuminant in PCS (s15Fixed16)
    write_u32(0x0000f6d6, 68, header);
    write_u32(0x00010000, 72, header);
    write_u32(0x0000d32d, 76, header);

    header[80..84].copy_from_slice(b"jxl "); // creator
}

fn finalize_tag(tags: &mut Vec<u8>, offset: &mut usize, size: &mut usize) {
    // Pad to 4-byte alignment
    while tags.len() % 4 != 0 {
        tags.push(0);
    }
    *offset += *size;
    *size = tags.len() - *offset;
}

fn add_to_tag_table(
    tag: &[u8; 4],
    offset: usize,
    size: usize,
    tagtable: &mut Vec<u8>,
    offsets: &mut Vec<usize>,
) {
    push_tag(tag, tagtable);
    push_u32(0, tagtable); // absolute offset placeholder
    offsets.push(offset);
    push_u32(size as u32, tagtable);
}

fn create_mluc_tag(text: &str, tags: &mut Vec<u8>) {
    push_tag(b"mluc", tags);
    push_u32(0, tags); // reserved
    push_u32(1, tags); // number of records
    push_u32(12, tags); // record size
    push_tag(b"enUS", tags);
    push_u32((text.len() * 2) as u32, tags); // string length in bytes
    push_u32(28, tags); // string offset
    for c in text.bytes() {
        tags.push(0); // high byte of UTF-16
        tags.push(c);
    }
}

fn create_xyz_tag(xyz: [f64; 3], tags: &mut Vec<u8>) {
    push_tag(b"XYZ ", tags);
    push_u32(0, tags); // reserved
    for v in xyz {
        push_s15fixed16(v, tags);
    }
}

fn create_chad_tag(tags: &mut Vec<u8>) {
    // Bradford chromatic adaptation from D65 to D50 (standard sRGB chad matrix)
    #[rustfmt::skip]
    let chad: [[f64; 3]; 3] = [
        [ 1.0479, 0.0229, -0.0502],
        [ 0.0296, 0.9904, -0.0171],
        [-0.0092, 0.0150,  0.7521],
    ];

    push_tag(b"sf32", tags);
    push_u32(0, tags); // reserved
    for row in &chad {
        for &v in row {
            push_s15fixed16(v, tags);
        }
    }
}

fn create_curv_para_tag(params: &[f64], curve_type: u16, tags: &mut Vec<u8>) {
    push_tag(b"para", tags);
    push_u32(0, tags); // reserved
    push_u16(curve_type, tags);
    push_u16(0, tags); // padding
    for &p in params {
        push_s15fixed16(p, tags);
    }
}

fn create_a2b0_tag_for_xyb(tags: &mut Vec<u8>) {
    let base = tags.len();

    push_tag(b"mAB ", tags); // signature
    push_u32(0, tags); // reserved
    push_u8(3, tags); // input channels
    push_u8(3, tags); // output channels
    push_u16(0, tags); // padding

    // Offsets from start of this tag data
    push_u32(32, tags); // B curves offset
    push_u32(244, tags); // matrix offset
    push_u32(148, tags); // M curves offset
    push_u32(80, tags); // CLUT offset
    push_u32(32, tags); // A curves offset (reuse B curves = identity)

    // --- offset 32: B/A curves (3 × identity parametric type 0, gamma=1.0) ---
    // Each: 'para'(4) + reserved(4) + type(2) + pad(2) + gamma(4) = 16 bytes
    // 3 curves × 16 = 48 bytes → ends at offset 80
    debug_assert_eq!(tags.len() - base, 32);
    create_curv_para_tag(&[1.0], 0, tags);
    create_curv_para_tag(&[1.0], 0, tags);
    create_curv_para_tag(&[1.0], 0, tags);

    // --- offset 80: CLUT ---
    debug_assert_eq!(tags.len() - base, 80);
    // Grid dimensions (16 bytes, first 3 are 2, rest 0)
    for i in 0..16 {
        push_u8(if i < 3 { 2 } else { 0 }, tags);
    }
    // Precision = 2 (uint16)
    push_u8(2, tags);
    // 3 bytes padding
    push_u8(0, tags);
    push_u16(0, tags);

    // 2×2×2×3 entries, uint16 each = 48 bytes
    // Compute the CLUT corners the same way jpegli does (kUnscaledA2BCube)
    for ix in 0..2usize {
        for iy in 0..2usize {
            for ib in 0..2usize {
                // XYBCorner: decode from grid position [0 or 1] back to XYB values
                let grid = [ix as f64, iy as f64, ib as f64];
                let mut xyb_corner = [0.0f64; 3];
                for c in 0..3 {
                    xyb_corner[c] = grid[c] / SCALED_XYB_SCALE[c] - SCALED_XYB_OFFSET[c];
                }

                // ScaledA2BCorner: undo XYB rotation
                let scaled_a2b = [
                    xyb_corner[1] + xyb_corner[0], // Y + X
                    xyb_corner[1] - xyb_corner[0], // Y - X
                    xyb_corner[2] + xyb_corner[1], // B + Y
                ];

                // UnscaledA2BCorner: apply offset and scale
                let out_f = [
                    (scaled_a2b[0] + XYB_OFFSET[0]) * XYB_SCALE[0],
                    (scaled_a2b[1] + XYB_OFFSET[1]) * XYB_SCALE[1],
                    (scaled_a2b[2] + XYB_OFFSET[2]) * XYB_SCALE[2],
                ];

                for &v in &out_f {
                    let val = (v * 65535.0).round().clamp(0.0, 65535.0) as u16;
                    push_u16(val, tags);
                }
            }
        }
    }

    // --- offset 148: M curves (3 × parametric type 3) ---
    // Each: 'para'(4) + reserved(4) + type(2) + pad(2) + 5×s15Fixed16(20) = 32 bytes
    // 3 curves × 32 = 96 bytes → ends at offset 244
    debug_assert_eq!(tags.len() - base, 148);
    for i in 0..3 {
        let b = -XYB_OFFSET[i] - (-NEG_OPSIN_ABSORBANCE_BIAS_RGB[i]).cbrt();
        let params = [
            3.0,                              // gamma (cube)
            1.0 / XYB_SCALE[i],               // a
            b,                                // b
            0.0,                              // c (output for x < d)
            f64::max(0.0, -b * XYB_SCALE[i]), // d (threshold)
        ];
        create_curv_para_tag(&params, 3, tags);
    }

    // --- offset 244: Matrix (3×3 + 3 offset = 12 s15Fixed16 = 48 bytes) ---
    debug_assert_eq!(tags.len() - base, 244);
    // Matrix values from jpegli's CreateICCLutAtoBTagForXYB
    #[rustfmt::skip]
    let matrix: [f64; 9] = [
         1.5170095, -1.1065225,  0.071623,
        -0.050022,   0.5683655, -0.018344,
        -1.387676,   1.1145555,  0.6857255,
    ];
    for v in matrix {
        push_s15fixed16(v, tags);
    }
    // Offset vector: matrix * (-bias)
    for i in 0..3 {
        let mut intercept = 0.0;
        for j in 0..3 {
            intercept += matrix[i * 3 + j] * NEG_OPSIN_ABSORBANCE_BIAS_RGB[j];
        }
        push_s15fixed16(intercept, tags);
    }
}

fn create_noop_b2a0_tag(tags: &mut Vec<u8>) {
    let base = tags.len();

    push_tag(b"mBA ", tags); // signature
    push_u32(0, tags); // reserved
    push_u8(3, tags); // input channels
    push_u8(3, tags); // output channels
    push_u16(0, tags); // padding

    push_u32(32, tags); // B curves offset
    push_u32(0, tags); // matrix offset (none)
    push_u32(0, tags); // M curves offset (none)
    push_u32(0, tags); // CLUT offset (none)
    push_u32(0, tags); // A curves offset (none)

    // 3 identity curves
    debug_assert_eq!(tags.len() - base, 32);
    create_curv_para_tag(&[1.0], 0, tags);
    create_curv_para_tag(&[1.0], 0, tags);
    create_curv_para_tag(&[1.0], 0, tags);
}

// ========================== MD5 (RFC 1321) for ICC Profile ID ===============

fn md5_digest(data: &[u8]) -> [u8; 16] {
    // Pad message
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    #[rustfmt::skip]
    static S: [u32; 64] = [
        7, 12, 17, 22,  7, 12, 17, 22,  7, 12, 17, 22,  7, 12, 17, 22,
        5,  9, 14, 20,  5,  9, 14, 20,  5,  9, 14, 20,  5,  9, 14, 20,
        4, 11, 16, 23,  4, 11, 16, 23,  4, 11, 16, 23,  4, 11, 16, 23,
        6, 10, 15, 21,  6, 10, 15, 21,  6, 10, 15, 21,  6, 10, 15, 21,
    ];

    #[rustfmt::skip]
    static K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }

        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                (a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g])).rotate_left(S[i]),
            );
            a = temp;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut result = [0u8; 16];
    result[0..4].copy_from_slice(&a0.to_le_bytes());
    result[4..8].copy_from_slice(&b0.to_le_bytes());
    result[8..12].copy_from_slice(&c0.to_le_bytes());
    result[12..16].copy_from_slice(&d0.to_le_bytes());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_srgb_to_linear() {
        assert!((srgb_to_linear(0.0) - 0.0).abs() < 1e-10);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-10);
        // 0.5 sRGB ≈ 0.214 linear
        assert!((srgb_to_linear(0.5) - 0.214).abs() < 0.001);
    }

    #[test]
    fn test_xyb_black() {
        let xyb = linear_rgb_to_scaled_xyb(0.0, 0.0, 0.0);
        for &v in &xyb {
            assert!(v >= 0.0 && v <= 1.0, "XYB value out of range: {v}");
        }
    }

    #[test]
    fn test_xyb_white() {
        let xyb = linear_rgb_to_scaled_xyb(1.0, 1.0, 1.0);
        for &v in &xyb {
            // Allow tiny floating-point overshoot; convert_to_xyb clamps before quantizing
            assert!(v >= -0.001 && v <= 1.001, "XYB value out of range: {v}");
        }
    }

    #[test]
    fn test_xyb_primaries_in_range() {
        // All sRGB primaries should produce XYB values in [0, 1]
        for (r, g, b) in [(1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)] {
            let xyb = linear_rgb_to_scaled_xyb(r, g, b);
            for &v in &xyb {
                assert!(
                    v >= -0.01 && v <= 1.01,
                    "XYB value out of range for ({r},{g},{b}): {v}"
                );
            }
        }
    }

    #[test]
    fn test_icc_profile_valid() {
        let icc = create_xyb_icc_profile();
        // Basic sanity: starts with correct size
        let size = u32::from_be_bytes([icc[0], icc[1], icc[2], icc[3]]) as usize;
        assert_eq!(size, icc.len());
        // Has 'acsp' signature at offset 36
        assert_eq!(&icc[36..40], b"acsp");
        // Profile version 4.4 at offset 8
        assert_eq!(icc[8], 0x04);
        assert_eq!(icc[9], 0x40);
    }

    #[test]
    fn test_md5() {
        // RFC 1321 test vector: md5("") = d41d8cd98f00b204e9800998ecf8427e
        let digest = md5_digest(b"");
        assert_eq!(
            digest,
            [
                0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
                0x42, 0x7e
            ]
        );
    }
}
