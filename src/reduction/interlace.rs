use std::borrow::Cow;

use crate::{headers::Orientation, png::PngImage, reduction::bit_depth::*};

/// Change interlacing or orientation, returning the new image if it was changed.
///
/// These two transforms are combined since reorientation must occur after deinterlacing but before
/// interlacing.
#[must_use]
pub fn restructured(png: &PngImage, interlace: Option<bool>, reorient: bool) -> Option<PngImage> {
    // Check if anything needs to be done
    let interlace = interlace.unwrap_or(png.ihdr.interlaced);
    let reorient = reorient && png.ihdr.orientation != Orientation::Normal;
    if interlace == png.ihdr.interlaced && !reorient {
        return None;
    }

    // Performing the transformations at the bit level can be complex and inefficient, so we only
    // directly support 8-bit and higher. A low depth image would normally already be expanded to 8
    // when we run this, but if depth reductions were disabled we can just expand it temporarily
    // and then revert back again afterward (it's still very fast).
    let orig_depth = png.ihdr.bit_depth;
    let mut png = Cow::Borrowed(png);
    if let Some(expanded) = expanded_bit_depth_to_8(&png) {
        png = Cow::Owned(expanded);
    }

    // Image must be deinterlaced to change orientation (we will reinterlace afterward if required)
    if png.ihdr.interlaced && (!interlace || reorient) {
        deinterlace_bytes(png.to_mut());
    }
    if reorient {
        reorient_image(png.to_mut());
    }
    if interlace {
        interlace_bytes(png.to_mut());
    }

    let mut new = png.into_owned();
    // Reduce back to original depth
    if new.ihdr.bit_depth != orig_depth {
        new = reduced_bit_depth_forced(&new, orig_depth);
    }
    Some(new)
}

/// Interlace by bytes, for images with at least 8bpp
fn interlace_bytes(png: &mut PngImage) {
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    png.data = match bytes_per_pixel {
        1 => interlace_bytes_const::<1>(png),
        2 => interlace_bytes_const::<2>(png),
        3 => interlace_bytes_const::<3>(png),
        4 => interlace_bytes_const::<4>(png),
        6 => interlace_bytes_const::<6>(png),
        8 => interlace_bytes_const::<8>(png),
        _ => unreachable!(),
    };
    png.ihdr.interlaced = true;
}

// Delegate function with const generics for performance
fn interlace_bytes_const<const BPP: usize>(png: &PngImage) -> Vec<u8> {
    let mut passes: Vec<Vec<u8>> = vec![Vec::new(); 7];
    for (y, line) in png.scan_lines(false).enumerate() {
        for (x, pixel) in line.data.as_chunks::<BPP>().0.iter().enumerate() {
            // Copy pixels into interlaced passes
            match (y % 8, x % 8) {
                (0, 0) => passes[0].extend_from_slice(pixel),
                (0, 4) => passes[1].extend_from_slice(pixel),
                (4, 0 | 4) => passes[2].extend_from_slice(pixel),
                (0 | 4, 2 | 6) => passes[3].extend_from_slice(pixel),
                (2 | 6, _) if x % 2 == 0 => passes[4].extend_from_slice(pixel),
                _ if y % 2 == 0 => passes[5].extend_from_slice(pixel),
                _ => passes[6].extend_from_slice(pixel),
            }
        }
    }
    passes.concat()
}

/// Deinterlace by bytes, for images with at least 8bpp
fn deinterlace_bytes(png: &mut PngImage) {
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    png.data = match bytes_per_pixel {
        1 => deinterlace_bytes_const::<1>(png),
        2 => deinterlace_bytes_const::<2>(png),
        3 => deinterlace_bytes_const::<3>(png),
        4 => deinterlace_bytes_const::<4>(png),
        6 => deinterlace_bytes_const::<6>(png),
        8 => deinterlace_bytes_const::<8>(png),
        _ => unreachable!(),
    };
    png.ihdr.interlaced = false;
}

// Delegate function with const generics for performance
fn deinterlace_bytes_const<const BPP: usize>(png: &PngImage) -> Vec<u8> {
    let bytes_per_pixel = BPP;
    let bytes_per_line = bytes_per_pixel * png.ihdr.width as usize;
    // Initialize the output data
    let mut data: Vec<u8> = vec![0; bytes_per_line * png.ihdr.height as usize];
    let mut current_pass = 1;
    let mut pass_constants = interlaced_constants(current_pass);
    let mut current_y: usize = pass_constants.y_shift as usize;
    for line in png.scan_lines(false) {
        for (i, pixel) in line.data.as_chunks::<BPP>().0.iter().enumerate() {
            let current_x = pass_constants.x_shift as usize + i * pass_constants.x_step as usize;
            // Copy this byte into the output line
            let index = current_y * bytes_per_line + current_x * bytes_per_pixel;
            data[index..(index + BPP)].copy_from_slice(pixel);
        }
        // Calculate the next line and move to next pass if necessary
        current_y += pass_constants.y_step as usize;
        if current_y >= png.ihdr.height as usize {
            if current_pass == 7 {
                break;
            }
            current_pass += 1;
            if current_pass == 2 && png.ihdr.width <= 4 {
                current_pass += 1;
            }
            if current_pass == 3 && png.ihdr.height <= 4 {
                current_pass += 1;
            }
            if current_pass == 4 && png.ihdr.width <= 2 {
                current_pass += 1;
            }
            if current_pass == 5 && png.ihdr.height <= 2 {
                current_pass += 1;
            }
            if current_pass == 6 && png.ihdr.width == 1 {
                current_pass += 1;
            }
            if current_pass == 7 && png.ihdr.height == 1 {
                break;
            }
            pass_constants = interlaced_constants(current_pass);
            current_y = pass_constants.y_shift as usize;
        }
    }
    data
}

#[derive(Clone, Copy)]
struct InterlacedConstants {
    x_shift: u8,
    y_shift: u8,
    x_step: u8,
    y_step: u8,
}

const fn interlaced_constants(pass: u8) -> InterlacedConstants {
    match pass {
        1 => InterlacedConstants {
            x_shift: 0,
            y_shift: 0,
            x_step: 8,
            y_step: 8,
        },
        2 => InterlacedConstants {
            x_shift: 4,
            y_shift: 0,
            x_step: 8,
            y_step: 8,
        },
        3 => InterlacedConstants {
            x_shift: 0,
            y_shift: 4,
            x_step: 4,
            y_step: 8,
        },
        4 => InterlacedConstants {
            x_shift: 2,
            y_shift: 0,
            x_step: 4,
            y_step: 4,
        },
        5 => InterlacedConstants {
            x_shift: 0,
            y_shift: 2,
            x_step: 2,
            y_step: 4,
        },
        6 => InterlacedConstants {
            x_shift: 1,
            y_shift: 0,
            x_step: 2,
            y_step: 2,
        },
        7 => InterlacedConstants {
            x_shift: 0,
            y_shift: 1,
            x_step: 1,
            y_step: 2,
        },
        _ => unreachable!(),
    }
}

/// Reorient the image according to its orientation
fn reorient_image(png: &mut PngImage) {
    match png.ihdr.orientation {
        Orientation::Normal => {}
        Orientation::FlipH => {
            png.data = flip_horizontal(png);
        }
        Orientation::Rot180 => {
            png.data = rotate_180(png);
        }
        Orientation::FlipV => {
            png.data = flip_vertical(png);
        }
        Orientation::FlipHRot270 => {
            png.data = flip_horizontal_rotate_270(png);
            std::mem::swap(&mut png.ihdr.width, &mut png.ihdr.height);
        }
        Orientation::Rot90 => {
            png.data = rotate_90(png);
            std::mem::swap(&mut png.ihdr.width, &mut png.ihdr.height);
        }
        Orientation::FlipHRot90 => {
            png.data = flip_horizontal_rotate_90(png);
            std::mem::swap(&mut png.ihdr.width, &mut png.ihdr.height);
        }
        Orientation::Rot270 => {
            png.data = rotate_270(png);
            std::mem::swap(&mut png.ihdr.width, &mut png.ihdr.height);
        }
    }
    png.ihdr.orientation = Orientation::Normal;
}

fn flip_horizontal(png: &mut PngImage) -> Vec<u8> {
    let w = png.ihdr.width as usize;
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    let line_len = bytes_per_pixel * w;
    let mut data = vec![0; png.data.len()];
    for (y, line) in png.data.chunks_exact(line_len).enumerate() {
        for (x, pixel) in line.chunks_exact(bytes_per_pixel).enumerate() {
            let index = y * line_len + (w - x - 1) * bytes_per_pixel;
            data[index..(index + bytes_per_pixel)].copy_from_slice(pixel);
        }
    }
    data
}

fn rotate_180(png: &mut PngImage) -> Vec<u8> {
    let w = png.ihdr.width as usize;
    let h = png.ihdr.height as usize;
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    let line_len = bytes_per_pixel * w;
    let mut data = vec![0; png.data.len()];
    for (y, line) in png.data.chunks_exact(line_len).enumerate() {
        for (x, pixel) in line.chunks_exact(bytes_per_pixel).enumerate() {
            let index = (h - y - 1) * line_len + (w - x - 1) * bytes_per_pixel;
            data[index..(index + bytes_per_pixel)].copy_from_slice(pixel);
        }
    }
    data
}

fn flip_vertical(png: &mut PngImage) -> Vec<u8> {
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    let line_len = bytes_per_pixel * png.ihdr.width as usize;
    png.data
        .chunks_exact(line_len)
        .rev()
        .flatten()
        .copied()
        .collect()
}

fn flip_horizontal_rotate_270(png: &mut PngImage) -> Vec<u8> {
    let w = png.ihdr.width as usize;
    let h = png.ihdr.height as usize;
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    let line_len = bytes_per_pixel * w;
    let new_line_len = bytes_per_pixel * h;
    let mut data = vec![0; png.data.len()];
    for (y, line) in png.data.chunks_exact(line_len).enumerate() {
        for (x, pixel) in line.chunks_exact(bytes_per_pixel).enumerate() {
            let index = x * new_line_len + y * bytes_per_pixel;
            data[index..(index + bytes_per_pixel)].copy_from_slice(pixel);
        }
    }
    data
}

fn rotate_90(png: &mut PngImage) -> Vec<u8> {
    let w = png.ihdr.width as usize;
    let h = png.ihdr.height as usize;
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    let line_len = bytes_per_pixel * w;
    let new_line_len = bytes_per_pixel * h;
    let mut data = vec![0; png.data.len()];
    for (y, line) in png.data.chunks_exact(line_len).enumerate() {
        for (x, pixel) in line.chunks_exact(bytes_per_pixel).enumerate() {
            let index = x * new_line_len + (h - y - 1) * bytes_per_pixel;
            data[index..(index + bytes_per_pixel)].copy_from_slice(pixel);
        }
    }
    data
}

fn flip_horizontal_rotate_90(png: &mut PngImage) -> Vec<u8> {
    let w = png.ihdr.width as usize;
    let h = png.ihdr.height as usize;
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    let line_len = bytes_per_pixel * w;
    let new_line_len = bytes_per_pixel * h;
    let mut data = vec![0; png.data.len()];
    for (y, line) in png.data.chunks_exact(line_len).enumerate() {
        for (x, pixel) in line.chunks_exact(bytes_per_pixel).enumerate() {
            let index = (w - x - 1) * new_line_len + (h - y - 1) * bytes_per_pixel;
            data[index..(index + bytes_per_pixel)].copy_from_slice(pixel);
        }
    }
    data
}

fn rotate_270(png: &mut PngImage) -> Vec<u8> {
    let w = png.ihdr.width as usize;
    let h = png.ihdr.height as usize;
    let bytes_per_pixel = png.ihdr.bpp() / 8;
    let line_len = bytes_per_pixel * w;
    let new_line_len = bytes_per_pixel * h;
    let mut data = vec![0; png.data.len()];
    for (y, line) in png.data.chunks_exact(line_len).enumerate() {
        for (x, pixel) in line.chunks_exact(bytes_per_pixel).enumerate() {
            let index = (w - x - 1) * new_line_len + y * bytes_per_pixel;
            data[index..(index + bytes_per_pixel)].copy_from_slice(pixel);
        }
    }
    data
}
