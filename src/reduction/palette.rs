use crate::colors::{BitDepth, ColorType};
use crate::headers::IhdrData;
use crate::png::PngImage;
use indexmap::map::{Entry::*, IndexMap};
use rgb::RGBA8;

/// Attempt to shrink and sort the palette, returning the optimized image if successful
#[must_use]
pub fn optimized_palette(png: &PngImage, optimize_alpha: bool) -> Option<PngImage> {
    let palette = match &png.ihdr.color_type {
        ColorType::Indexed { palette } => palette,
        // Can't reduce if there is no palette
        _ => return None,
    };
    if png.ihdr.bit_depth == BitDepth::One {
        // Gains from 1-bit images will be at most 1 byte
        // Not worth the CPU time
        return None;
    }

    let mut palette_map = [None; 256];
    let mut used = [false; 256];
    {
        // Find palette entries that are never used
        match png.ihdr.bit_depth {
            BitDepth::Eight => {
                for &byte in &png.data {
                    used[byte as usize] = true;
                }
            }
            BitDepth::Four => {
                for &byte in &png.data {
                    used[(byte & 0x0F) as usize] = true;
                    used[(byte >> 4) as usize] = true;
                }
            }
            BitDepth::Two => {
                for &byte in &png.data {
                    used[(byte & 0x03) as usize] = true;
                    used[((byte >> 2) & 0x03) as usize] = true;
                    used[((byte >> 4) & 0x03) as usize] = true;
                    used[(byte >> 6) as usize] = true;
                }
            }
            _ => unreachable!(),
        }

        let mut used_enumerated: Vec<(usize, &bool)> = used.iter().enumerate().collect();
        used_enumerated.sort_by(|a, b| {
            //Sort by ascending alpha and descending luma.
            let color_val = |i| {
                let color = palette
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| RGBA8::new(0, 0, 0, 255));
                ((color.a as i32) << 18)
                // These are coefficients for standard sRGB to luma conversion
                - i32::from(color.r) * 299
                - i32::from(color.g) * 587
                - i32::from(color.b) * 114
            };
            color_val(a.0).cmp(&color_val(b.0))
        });

        // Make sure the background is also included, but only after sorting since it may not be used in idat
        if let Some(&idx) = png.aux_headers.get(b"bKGD").and_then(|b| b.first()) {
            if !used[idx as usize] {
                used_enumerated.push((idx as usize, &true));
            }
        }

        let mut next_index = 0_u16;
        let mut seen = IndexMap::with_capacity(palette.len());
        for (i, used) in used_enumerated.iter().cloned() {
            if !used {
                continue;
            }
            // There are invalid files that use pixel indices beyond palette size
            let mut color = palette
                .get(i)
                .cloned()
                .unwrap_or_else(|| RGBA8::new(0, 0, 0, 255));
            // If there are multiple fully transparent entries, reduce them into one
            if optimize_alpha && color.a == 0 {
                color.r = 0;
                color.g = 0;
                color.b = 0;
            }
            match seen.entry(color) {
                Vacant(new) => {
                    palette_map[i] = Some(next_index as u8);
                    new.insert(next_index as u8);
                    next_index += 1;
                }
                Occupied(remap_to) => palette_map[i] = Some(*remap_to.get()),
            }
        }
    }

    do_palette_reduction(png, palette, &palette_map)
}

#[must_use]
fn do_palette_reduction(
    png: &PngImage,
    palette: &[RGBA8],
    palette_map: &[Option<u8>; 256],
) -> Option<PngImage> {
    let byte_map = palette_map_to_byte_map(png, palette_map)?;

    // Reassign data bytes to new indices
    let raw_data = png.data.iter().map(|b| byte_map[*b as usize]).collect();

    let mut aux_headers = png.aux_headers.clone();
    if let Some(bkgd_header) = png.aux_headers.get(b"bKGD") {
        if let Some(Some(map_to)) = bkgd_header
            .first()
            .and_then(|&idx| palette_map.get(idx as usize))
        {
            aux_headers.insert(*b"bKGD", vec![*map_to]);
        }
    }

    Some(PngImage {
        ihdr: IhdrData {
            color_type: ColorType::Indexed {
                palette: reordered_palette(palette, palette_map),
            },
            ..png.ihdr
        },
        data: raw_data,
        aux_headers,
    })
}

fn palette_map_to_byte_map(png: &PngImage, palette_map: &[Option<u8>; 256]) -> Option<[u8; 256]> {
    if (0..256).all(|i| palette_map[i].map_or(true, |to| to == i as u8)) {
        // No reduction necessary
        return None;
    }

    let mut byte_map = [0_u8; 256];

    // low bit-depths can be pre-computed for every byte value
    match png.ihdr.bit_depth {
        BitDepth::Eight => {
            for byte in 0..=255usize {
                byte_map[byte] = palette_map[byte].unwrap_or(0)
            }
        }
        BitDepth::Four => {
            for byte in 0..=255usize {
                byte_map[byte] = palette_map[byte & 0x0F].unwrap_or(0)
                    | (palette_map[byte >> 4].unwrap_or(0) << 4);
            }
        }
        BitDepth::Two => {
            for byte in 0..=255usize {
                byte_map[byte] = palette_map[byte & 0x03].unwrap_or(0)
                    | (palette_map[(byte >> 2) & 0x03].unwrap_or(0) << 2)
                    | (palette_map[(byte >> 4) & 0x03].unwrap_or(0) << 4)
                    | (palette_map[byte >> 6].unwrap_or(0) << 6);
            }
        }
        _ => {}
    }

    Some(byte_map)
}

fn reordered_palette(palette: &[RGBA8], palette_map: &[Option<u8>; 256]) -> Vec<RGBA8> {
    let max_index = palette_map.iter().cloned().flatten().max().unwrap_or(0) as usize;
    let mut new_palette = vec![RGBA8::new(0, 0, 0, 255); max_index + 1];
    for (&color, &map_to) in palette.iter().zip(palette_map.iter()) {
        if let Some(map_to) = map_to {
            new_palette[map_to as usize] = color;
        }
    }
    new_palette
}