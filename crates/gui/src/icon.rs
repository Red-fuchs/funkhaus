/// Funkhaus icon: rounded square in warm gradient, white mic dot with radio waves. RGBA, `n`×`n`.
pub fn rgba(n: u32) -> Vec<u8> {
    let ss = 4; // supersampling
    let mut px = vec![0u8; (n * n * 4) as usize];
    let f = n as f32;
    for y in 0..n {
        for x in 0..n {
            let (mut acc, mut cov) = ([0f32; 3], 0f32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let u = (x as f32 + (sx as f32 + 0.5) / ss as f32) / f;
                    let v = (y as f32 + (sy as f32 + 0.5) / ss as f32) / f;
                    if let Some(c) = shade(u, v) {
                        acc.iter_mut().zip(c).for_each(|(a, c)| *a += c);
                        cov += 1.0;
                    }
                }
            }
            let i = ((y * n + x) * 4) as usize;
            if cov > 0.0 {
                for k in 0..3 {
                    px[i + k] = (acc[k] / cov * 255.0) as u8;
                }
                px[i + 3] = (cov / (ss * ss) as f32 * 255.0) as u8;
            }
        }
    }
    px
}

fn shade(u: f32, v: f32) -> Option<[f32; 3]> {
    // macOS-style squircle-ish rounded rect with margin
    let (m, r) = (0.09, 0.2);
    let (dx, dy) = ((u - 0.5).abs() - (0.5 - m - r), (v - 0.5).abs() - (0.5 - m - r));
    if (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() > r {
        return None;
    }
    let t = v;
    let bg = [0.98 - 0.25 * t, 0.55 - 0.35 * t, 0.18 + 0.12 * t];
    let (cx, cy) = (0.5, 0.6);
    let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
    let ang = (v - cy).atan2(u - cx);
    let up = ang < -0.6 && ang > -2.54; // upper wedge for waves
    let white = [1.0, 0.97, 0.92];
    if d < 0.07 {
        return Some(white);
    }
    if up {
        for rr in [0.16, 0.25, 0.34] {
            if (d - rr).abs() < 0.028 {
                return Some(white);
            }
        }
    }
    // mast
    if (u - cx).abs() < 0.022 && v > cy && v < 0.82 {
        return Some(white);
    }
    if (v - 0.82).abs() < 0.022 && (u - cx).abs() < 0.12 {
        return Some(white);
    }
    Some(bg)
}

pub fn write_png(path: &std::path::Path, n: u32) -> anyhow::Result<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut e = png::Encoder::new(f, n, n);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()?.write_image_data(&rgba(n))?;
    Ok(())
}
