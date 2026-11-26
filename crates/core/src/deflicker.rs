//! Deflicker: each frame's brightness (and colour) measured, a steady level
//! found by smoothing over a few frames, and the gain that takes each frame to
//! that level (for the Deflicker effect's keyframes). Flicker from lights,
//! timelapses' changing exposure, and old film's uneven frames go; slow
//! changes (a cloud passing, a fade) stay.

const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// For each frame's average colour (`means`, linear light), its gains as the
/// Deflicker effect's settings: brightness, red, green and blue (%). Levels are
/// smoothed over about `smoothing` frames; with `colour`, the colour's balance
/// is evened out too (otherwise the colour gains stay at 100 %).
pub fn gains(means: &[[f32; 3]], smoothing: f32, colour: bool) -> Vec<[f32; 4]> {
    let n = means.len();
    if n == 0 {
        return Vec::new();
    }
    let floor = 1e-5f32;
    let luma: Vec<f32> = means.iter().map(|m| (m[0] * LUMA[0] + m[1] * LUMA[1] + m[2] * LUMA[2]).max(floor)).collect();
    // Brightness in stops; colour as each channel's share of it.
    let level: Vec<[f32; 4]> = means
        .iter()
        .zip(&luma)
        .map(|(m, &y)| [y.ln(), (m[0].max(floor) / y).ln(), (m[1].max(floor) / y).ln(), (m[2].max(floor) / y).ln()])
        .collect();
    let sigma = (if smoothing.is_finite() { smoothing } else { 15.0 }).clamp(1.0, 1000.0) / 2.0;
    let reach = (sigma * 3.0).ceil() as i64;
    let last = n as i64 - 1;
    (0..n as i64)
        .map(|i| {
            let (mut sum, mut wsum) = ([0.0f32; 4], 0.0f32);
            for j in (i - reach)..=(i + reach) {
                let Some(l) = level.get(j.clamp(0, last) as usize) else { continue };
                let w = (-((j - i) as f32).powi(2) / (2.0 * sigma * sigma)).exp();
                for (s, v) in sum.iter_mut().zip(l) {
                    *s += v * w;
                }
                wsum += w;
            }
            let steady = sum.map(|s| s / wsum.max(1e-12));
            let Some(now) = level.get(i as usize) else { return [100.0; 4] };
            let pct = |k: usize| ((steady[k] - now[k]).exp() * 100.0).clamp(0.0, 400.0);
            if colour {
                [pct(0), pct(1), pct(2), pct(3)]
            } else {
                [pct(0), 100.0, 100.0, 100.0]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evens_out_flicker_and_keeps_slow_changes() {
        // A slow fade up, with every other frame 30 % brighter and a bit redder.
        let means: Vec<[f32; 3]> = (0..80)
            .map(|i| {
                let base = 0.1 + i as f32 * 0.002;
                let (k, r) = if i % 2 == 0 { (1.3, 1.1) } else { (1.0, 1.0) };
                [base * k * r, base * k, base * k]
            })
            .collect();
        let g = gains(&means, 10.0, true);
        assert_eq!(g.len(), 80);
        let out: Vec<[f32; 3]> = means.iter().zip(&g).map(|(m, g)| {
            let b = g[0] / 100.0;
            [m[0] * b * g[1] / 100.0, m[1] * b * g[2] / 100.0, m[2] * b * g[3] / 100.0]
        }).collect();
        for i in 20..60 {
            // Neighbours now match (to a couple of percent)…
            let (a, b) = (out[i], out[i + 1]);
            assert!((a[1] / b[1] - 1.0).abs() < 0.03, "frame {i}: {a:?} vs {b:?}");
            assert!((a[0] / a[1] - b[0] / b[1]).abs() < 0.02, "colour at {i}");
        }
        // …and the fade is still there.
        assert!(out[70][1] > out[10][1] * 1.4);
        // Without colour, only brightness moves.
        assert!(gains(&means, 10.0, false).iter().all(|g| g[1] == 100.0 && g[3] == 100.0));
    }

    #[test]
    fn odd_input_does_no_harm() {
        assert!(gains(&[], 10.0, true).is_empty());
        let g = gains(&[[0.0; 3], [f32::NAN, 1.0, 0.0], [1e9, 1e9, 1e9]], f32::NAN, true);
        assert_eq!(g.len(), 3);
        assert!(g.iter().flatten().all(|v| (0.0..=400.0).contains(v)));
    }
}
