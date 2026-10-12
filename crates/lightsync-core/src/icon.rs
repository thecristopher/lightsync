//! The app's icon, drawn in code so it's crisp at every size and there's no image file to keep in
//! sync: the tray and windows draw it at runtime, and the build turns it into the exe's .ico.

/// A dark screen with a rainbow glow around it, like lights behind a monitor. RGBA rows.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    // 4×4 samples per pixel, so the rounded edges stay smooth at 256 px too
    const GRID: u32 = 4;
    let s = size as f32 / 32.0;
    let inside = |x: f32, y: f32, (l, t, r, b): (f32, f32, f32, f32), radius: f32| {
        let cx = x.clamp(l + radius, r - radius);
        let cy = y.clamp(t + radius, b - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius && x >= l && x <= r && y >= t && y <= b
    };
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut sum, mut covered) = ([0.0f32; 3], 0.0f32);
            for sy in 0..GRID {
                for sx in 0..GRID {
                    // In the 32 unit design space
                    let fx = (x as f32 + (sx as f32 + 0.5) / GRID as f32) / s;
                    let fy = (y as f32 + (sy as f32 + 0.5) / GRID as f32) / s;
                    if !inside(fx, fy, (0.0, 0.0, 32.0, 32.0), 7.0) {
                        continue;
                    }
                    let hue = ((fy - 16.0).atan2(fx - 16.0) / std::f32::consts::TAU + 1.0) % 1.0;
                    let mut color = hsv(hue, 0.75, 1.0);
                    if inside(fx, fy, (6.0, 8.5, 26.0, 23.5), 2.5) {
                        color = [255.0, 255.0, 255.0];
                    }
                    if inside(fx, fy, (7.8, 10.3, 24.2, 21.7), 1.2) {
                        color = [28.0, 30.0, 36.0];
                    }
                    for c in 0..3 {
                        sum[c] += color[c];
                    }
                    covered += 1.0;
                }
            }
            if covered > 0.0 {
                let i = ((y * size + x) * 4) as usize;
                let alpha = covered / (GRID * GRID) as f32;
                pixels[i..i + 4].copy_from_slice(&[
                    (sum[0] / covered).round() as u8,
                    (sum[1] / covered).round() as u8,
                    (sum[2] / covered).round() as u8,
                    (alpha * 255.0).round() as u8,
                ]);
            }
        }
    }
    pixels
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r * 255.0, g * 255.0, b * 255.0]
}

/// A Windows .ico holding the icon at each of `sizes`, as 32-bit images with alpha.
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| dib(size)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// One icon image: a BITMAPINFOHEADER, BGRA rows bottom up, then an empty AND mask.
fn dib(size: u32) -> Vec<u8> {
    let rgba = icon_rgba(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    for value in [40u32, size, size * 2] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(size * size * 4 + mask_row * size).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_has_a_dark_screen_in_the_middle() {
        let icon = icon_rgba(32);
        let center = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(&icon[center..center + 4], &[28, 30, 36, 255]);
        assert_eq!(icon[3], 0, "the rounded corner stays transparent");
    }

    #[test]
    fn the_ico_lists_every_size() {
        let ico = ico(&[16, 256]);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(ico[6], 16);
        assert_eq!(ico[22], 0, "256 is written as 0");
        let first = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(first, 6 + 32);
        assert_eq!(u32::from_le_bytes(ico[first + 8..first + 12].try_into().unwrap()), 32, "height covers the mask");
    }
}
