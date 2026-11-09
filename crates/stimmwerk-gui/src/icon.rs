//! App icon, procedural (no asset): brass→walnut rounded square, cream speech bubble with a walnut tuning fork.
//! Same family/rasteriser as funkhaus' icon; fork strokes widened from the draft (0.022 → 0.034) so the tines survive at 32 px.

/// RGBA, `n`×`n`, 4×4 supersampled.
pub fn rgba(n: u32) -> Vec<u8> {
    let ss = 4;
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
    let (m, r) = (0.09, 0.2);
    let (dx, dy) = ((u - 0.5).abs() - (0.5 - m - r), (v - 0.5).abs() - (0.5 - m - r));
    if (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() > r {
        return None;
    }
    let bg = [0.93 - 0.45 * v, 0.70 - 0.40 * v, 0.30 - 0.12 * v];
    let (wood, cream) = ([0.22, 0.13, 0.08], [1.0, 0.96, 0.88]);
    let bubble = sd_rrect(u, v, [0.5, 0.45], [0.28, 0.2], 0.09) < 0.0 || in_tri([u, v], [0.33, 0.6], [0.50, 0.6], [0.28, 0.76]);
    if !bubble {
        return Some(bg);
    }
    let w = 0.034;
    let fork = (u - 0.5).abs() < w && (0.43..0.6).contains(&v) // stem
        || (v - 0.43).abs() < w && (u - 0.5).abs() < 0.1 // bend bar
        || [0.42, 0.58].iter().any(|&x| (u - x).abs() < w && (0.3..0.43).contains(&v)); // tines
    Some(if fork { wood } else { cream })
}

/// Signed distance to a rounded rect (negative inside).
fn sd_rrect(u: f32, v: f32, c: [f32; 2], h: [f32; 2], r: f32) -> f32 {
    let (qx, qy) = ((u - c[0]).abs() - h[0] + r, (v - c[1]).abs() - h[1] + r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

fn in_tri(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let s = |p: [f32; 2], a: [f32; 2], b: [f32; 2]| (p[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (p[1] - b[1]);
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

pub fn write_png(path: &std::path::Path, n: u32) -> anyhow::Result<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut e = png::Encoder::new(f, n, n);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()?.write_image_data(&rgba(n))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// At 32 px both tines stay visible: two dark columns separated by cream on the tine row.
    #[test]
    fn tines_survive_32px() {
        let n = 32;
        let px = super::rgba(n);
        let y = (0.36 * n as f32) as u32;
        let dark: Vec<bool> = (0..n).map(|x| ((y * n + x) * 4) as usize).map(|i| px[i + 3] > 200 && px[i] < 140).collect();
        let runs = dark.windows(2).filter(|w| !w[0] && w[1]).count();
        assert_eq!(runs, 2, "{dark:?}");
    }
}
