//! Procedurally drawn app icon (a mouse on a rounded tile).
//!
//! Dependency-free on purpose: `build.rs` includes this file to generate the
//! Windows `.ico`, and the app uses it at runtime for the tray and window icons.

/// Renders a `size`×`size` icon as straight (non-premultiplied) RGBA bytes.
/// `active = false` draws the grey "disabled" variant.
pub fn render_rgba(size: u32, active: bool) -> Vec<u8> {
    const SS: u32 = 4; // supersampling per axis
    let (top, bottom) = if active {
        ([0x5B, 0x8C, 0xFF], [0x2F, 0x5B, 0xE0])
    } else {
        ([0x9A, 0xA0, 0xA8], [0x6E, 0x74, 0x7D])
    };

    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut tile, mut body, mut wheel) = (0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f64 + (sx as f64 + 0.5) / SS as f64) / size as f64;
                    let y = (py as f64 + (sy as f64 + 0.5) / SS as f64) / size as f64;
                    if round_box(x, y, 0.5, 0.5, 0.47, 0.47, 0.22) <= 0.0 {
                        tile += 1.0;
                    }
                    if round_box(x, y, 0.5, 0.53, 0.215, 0.305, 0.205) <= 0.0 {
                        body += 1.0;
                    }
                    if round_box(x, y, 0.5, 0.385, 0.05, 0.095, 0.05) <= 0.0 {
                        wheel += 1.0;
                    }
                }
            }
            let n = (SS * SS) as f64;
            let (tile, body, wheel) = (tile / n, body / n, wheel / n);

            let t = py as f64 / size.max(1) as f64;
            let bg = [0, 1, 2].map(|i| lerp(top[i] as f64, bottom[i] as f64, t) / 255.0);

            // Composite in premultiplied space: tile, then white body, then wheel in the tile colour.
            let mut rgb = bg.map(|c| c * tile);
            let mut a = tile;
            for (layer, alpha) in [([1.0, 1.0, 1.0], body), (bg, wheel)] {
                for i in 0..3 {
                    rgb[i] = layer[i] * alpha + rgb[i] * (1.0 - alpha);
                }
                a = alpha + a * (1.0 - alpha);
            }

            for c in rgb {
                let straight = if a > 0.0 { c / a } else { 0.0 };
                out.push((straight * 255.0).round().clamp(0.0, 255.0) as u8);
            }
            out.push((a * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    out
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Signed distance to a rounded box centred at (cx, cy) with half-size (hx, hy).
fn round_box(x: f64, y: f64, cx: f64, cy: f64, hx: f64, hy: f64, r: f64) -> f64 {
    let qx = (x - cx).abs() - (hx - r);
    let qy = (y - cy).abs() - (hy - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}
