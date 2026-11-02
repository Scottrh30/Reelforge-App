//! The processing behind clip audio effects (see `reelforge_core::audiofx`):
//! stereo, interleaved, block by block, with state carried between blocks.
//!
//! - **EQ**: RBJ biquads (a 12 dB/octave low cut, low and high shelves, a bell).
//! - **Compressor**: a stereo-linked peak detector, a soft-knee gain curve, and
//!   attack/release smoothing of the gain reduction in dB.
//! - **Limiter**: 5 ms of look-ahead (the gain is already down when a peak
//!   arrives), release smoothing, and a hard ceiling as a last guard.
//! - **Reverb**: Freeverb (eight damped combs and four all-passes per side,
//!   the right side's delays slightly longer for width) after a pre-delay.
//! - **Noise reduction**: spectral gating on a 1024-point STFT (hop 256,
//!   √Hann windows). Each frequency's noise floor is tracked as a slowly
//!   rising minimum of its smoothed power, so it learns the noise as it plays;
//!   each bin is turned down by how little it rises above that floor
//!   (Wiener-style), with the gains smoothed over time and frequency so the
//!   leftover noise doesn't twinkle.
//!
//! Effects with look-ahead (limiter, noise reduction) delay their output;
//! [`FxChain::latency`] says by how much, and the mixer feeds the chain that
//! far ahead so the sound stays in sync.

use reelforge_core::audiofx::{comp, denoise, eq, limit, reverb, AudioFxKind};
use std::collections::VecDeque;
use std::f32::consts::PI;

const CH: usize = 2;

fn db(g: f32) -> f32 {
    20.0 * g.max(1e-9).log10()
}
fn gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}
/// A one-pole smoothing coefficient for a time constant of `ms`.
fn coef(ms: f32, rate: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * rate)).exp()
}

/// One processing stage.
trait Proc: Send {
    /// Process `buf` (interleaved stereo) in place with these parameter values.
    fn process(&mut self, buf: &mut [f32], p: &[f32]);
    /// Samples its output lags its input by.
    fn latency(&self) -> usize {
        0
    }
    /// How much sound before a jump it needs to settle (seconds).
    fn settle(&self) -> f64;
}

// ---- EQ -------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    /// Direct form II transposed state, per channel.
    z: [[f32; 2]; CH],
}

impl Biquad {
    fn set(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        self.b = [b0 / a0, b1 / a0, b2 / a0];
        self.a = [a1 / a0, a2 / a0];
    }
    fn run(&mut self, x: f32, ch: usize) -> f32 {
        let z = &mut self.z[ch];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
    fn highpass(&mut self, f: f32, rate: f32) {
        let w = 2.0 * PI * f / rate;
        let (c, al) = (w.cos(), w.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2));
        self.set((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al);
    }
    fn shelf(&mut self, f: f32, gain_db: f32, rate: f32, high: bool) {
        let a = 10f32.powf(gain_db / 40.0);
        let w = 2.0 * PI * f / rate;
        let (c, s) = (w.cos(), w.sin());
        let al = s / 2.0 * std::f32::consts::SQRT_2;
        let k = 2.0 * a.sqrt() * al;
        if high {
            self.set(a * ((a + 1.0) + (a - 1.0) * c + k), -2.0 * a * ((a - 1.0) + (a + 1.0) * c), a * ((a + 1.0) + (a - 1.0) * c - k), (a + 1.0) - (a - 1.0) * c + k, 2.0 * ((a - 1.0) - (a + 1.0) * c), (a + 1.0) - (a - 1.0) * c - k);
        } else {
            self.set(a * ((a + 1.0) - (a - 1.0) * c + k), 2.0 * a * ((a - 1.0) - (a + 1.0) * c), a * ((a + 1.0) - (a - 1.0) * c - k), (a + 1.0) + (a - 1.0) * c + k, -2.0 * ((a - 1.0) + (a + 1.0) * c), (a + 1.0) + (a - 1.0) * c - k);
        }
    }
    fn bell(&mut self, f: f32, gain_db: f32, q: f32, rate: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w = 2.0 * PI * f / rate;
        let al = w.sin() / (2.0 * q.max(0.05));
        let c = w.cos();
        self.set(1.0 + al * a, -2.0 * c, 1.0 - al * a, 1.0 + al / a, -2.0 * c, 1.0 - al / a);
    }
}

struct Eq {
    rate: f32,
    last: Vec<f32>,
    cut: Biquad,
    low: Biquad,
    mid: Biquad,
    high: Biquad,
}

impl Proc for Eq {
    fn process(&mut self, buf: &mut [f32], p: &[f32]) {
        let nyq = self.rate * 0.45;
        if self.last != p {
            self.cut.highpass(p[eq::LOW_CUT].min(nyq), self.rate);
            self.low.shelf(p[eq::LOW_FREQ].min(nyq), p[eq::LOW_GAIN], self.rate, false);
            self.mid.bell(p[eq::MID_FREQ].min(nyq), p[eq::MID_GAIN], p[eq::MID_Q], self.rate);
            self.high.shelf(p[eq::HIGH_FREQ].min(nyq), p[eq::HIGH_GAIN], self.rate, true);
            self.last = p.to_vec();
        }
        let cut = p[eq::LOW_CUT] > 20.5;
        let (lo, mi, hi) = (p[eq::LOW_GAIN] != 0.0, p[eq::MID_GAIN] != 0.0, p[eq::HIGH_GAIN] != 0.0);
        for f in buf.chunks_exact_mut(CH) {
            for (ch, x) in f.iter_mut().enumerate() {
                let mut v = *x;
                if cut {
                    v = self.cut.run(v, ch);
                }
                if lo {
                    v = self.low.run(v, ch);
                }
                if mi {
                    v = self.mid.run(v, ch);
                }
                if hi {
                    v = self.high.run(v, ch);
                }
                *x = v;
            }
        }
    }
    fn settle(&self) -> f64 {
        0.1
    }
}

// ---- Compressor -------------------------------------------------------------------

struct Compressor {
    rate: f32,
    /// Gain reduction now, dB (≥ 0).
    gr: f32,
}

/// The gain reduction (dB) a soft-knee compressor asks for at input level `x` dB.
fn reduction(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - threshold;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if knee > 0.0 && over.abs() <= knee / 2.0 {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

impl Proc for Compressor {
    fn process(&mut self, buf: &mut [f32], p: &[f32]) {
        let (att, rel) = (coef(p[comp::ATTACK], self.rate), coef(p[comp::RELEASE], self.rate));
        let makeup = p[comp::MAKEUP];
        for f in buf.chunks_exact_mut(CH) {
            let level = db(f[0].abs().max(f[1].abs()));
            let want = reduction(level, p[comp::THRESHOLD], p[comp::RATIO], p[comp::KNEE]);
            let k = if want > self.gr { att } else { rel };
            self.gr = want + (self.gr - want) * k;
            let g = gain(makeup - self.gr);
            f[0] *= g;
            f[1] *= g;
        }
    }
    fn settle(&self) -> f64 {
        0.5
    }
}

// ---- Limiter ------------------------------------------------------------------------

struct Limiter {
    rate: f32,
    look: usize,
    delay: VecDeque<[f32; 2]>,
    /// The gain each sample in the look-ahead needs, with a running minimum.
    needs: VecDeque<(usize, f32)>,
    n: usize,
    env: f32,
}

impl Limiter {
    fn new(rate: f32) -> Self {
        let look = (rate * 0.005) as usize;
        Limiter { rate, look, delay: std::iter::repeat_n([0.0; 2], look).collect(), needs: VecDeque::new(), n: 0, env: 1.0 }
    }
}

impl Proc for Limiter {
    fn process(&mut self, buf: &mut [f32], p: &[f32]) {
        let drive = gain(p[limit::GAIN]);
        let ceil = gain(p[limit::CEILING]);
        let rel = coef(p[limit::RELEASE], self.rate);
        // Coming down over most of the look-ahead, so a peak arrives at the gain it needs.
        let att = coef(self.look as f32 / self.rate * 1000.0 / 4.0, self.rate);
        for f in buf.chunks_exact_mut(CH) {
            let x = [f[0] * drive, f[1] * drive];
            let peak = x[0].abs().max(x[1].abs());
            let need = if peak > ceil { ceil / peak } else { 1.0 };
            // Monotonic queue: the smallest need over the look-ahead window.
            while self.needs.back().is_some_and(|&(_, g)| g >= need) {
                self.needs.pop_back();
            }
            self.needs.push_back((self.n, need));
            while self.needs.front().is_some_and(|&(i, _)| i + self.look < self.n) {
                self.needs.pop_front();
            }
            let target = self.needs.front().map_or(1.0, |&(_, g)| g);
            self.env = if target < self.env { target + (self.env - target) * att } else { target + (self.env - target) * rel };
            self.delay.push_back(x);
            let y = self.delay.pop_front().unwrap_or([0.0; 2]);
            // A last guard: never over the ceiling.
            f[0] = (y[0] * self.env).clamp(-ceil, ceil);
            f[1] = (y[1] * self.env).clamp(-ceil, ceil);
            self.n += 1;
        }
    }
    fn latency(&self) -> usize {
        self.look
    }
    fn settle(&self) -> f64 {
        0.2
    }
}

// ---- Reverb (Freeverb) ----------------------------------------------------------------

struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
}
impl Comb {
    fn run(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.i];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.i] = x + self.store * feedback;
        self.i = (self.i + 1) % self.buf.len();
        y
    }
}
struct Allpass {
    buf: Vec<f32>,
    i: usize,
}
impl Allpass {
    fn run(&mut self, x: f32) -> f32 {
        let b = self.buf[self.i];
        let y = b - x;
        self.buf[self.i] = x + b * 0.5;
        self.i = (self.i + 1) % self.buf.len();
        y
    }
}

struct Reverb {
    rate: f32,
    combs: [Vec<Comb>; CH],
    passes: [Vec<Allpass>; CH],
    pre: VecDeque<[f32; 2]>,
}

impl Reverb {
    fn new(rate: f32) -> Self {
        let k = rate / 44_100.0;
        let len = |n: usize, side: usize| (((n + side * 23) as f32 * k) as usize).max(1);
        let combs = [0, 1].map(|s| [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617].iter().map(|&n| Comb { buf: vec![0.0; len(n, s)], i: 0, store: 0.0 }).collect());
        let passes = [0, 1].map(|s| [556, 441, 341, 225].iter().map(|&n| Allpass { buf: vec![0.0; len(n, s)], i: 0 }).collect());
        Reverb { rate, combs, passes, pre: VecDeque::new() }
    }
}

impl Proc for Reverb {
    fn process(&mut self, buf: &mut [f32], p: &[f32]) {
        let feedback = 0.7 + 0.28 * p[reverb::SIZE] / 100.0;
        let damp = 0.4 * p[reverb::DAMPING] / 100.0;
        let width = p[reverb::WIDTH] / 100.0;
        let mix = p[reverb::MIX] / 100.0;
        let (wet1, wet2) = (3.0 * (width / 2.0 + 0.5), 3.0 * ((1.0 - width) / 2.0));
        let pre = (p[reverb::PREDELAY] * 0.001 * self.rate) as usize;
        for f in buf.chunks_exact_mut(CH) {
            self.pre.push_back([f[0], f[1]]);
            while self.pre.len() > pre + 1 {
                self.pre.pop_front();
            }
            let d = if self.pre.len() > pre { self.pre[0] } else { [0.0; 2] };
            let input = (d[0] + d[1]) * 0.015;
            let mut out = [0.0f32; 2];
            for (ch, o) in out.iter_mut().enumerate() {
                let mut acc = 0.0;
                for c in &mut self.combs[ch] {
                    acc += c.run(input, feedback, damp);
                }
                for a in &mut self.passes[ch] {
                    acc = a.run(acc);
                }
                *o = acc;
            }
            let (l, r) = (out[0] * wet1 + out[1] * wet2, out[1] * wet1 + out[0] * wet2);
            f[0] = f[0] * (1.0 - mix) + l * mix;
            f[1] = f[1] * (1.0 - mix) + r * mix;
        }
    }
    fn settle(&self) -> f64 {
        2.0
    }
}

// ---- Noise reduction ----------------------------------------------------------------

/// STFT size and hop for noise reduction.
const NR_N: usize = 1024;
const NR_HOP: usize = 256;
/// How far a running minimum of smoothed noise power sits below its average.
const NR_BIAS: f32 = 2.5;

/// In-place iterative radix-2 FFT of `re`/`im` (length a power of two); the
/// inverse when `inverse` (unscaled).
fn fft(re: &mut [f32], im: &mut [f32], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * std::f64::consts::PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        let (wr, wi) = (ang.cos() as f32, ang.sin() as f32);
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}

struct Denoise {
    rate: f32,
    window: Vec<f32>,
    /// The last `NR_N` input samples, per channel.
    hist: [Vec<f32>; CH],
    /// Overlap-add accumulator, per channel.
    acc: [Vec<f32>; CH],
    pending: usize,
    out: VecDeque<[f32; 2]>,
    /// Per bin: smoothed power, the noise floor, the gain last frame.
    power: Vec<f32>,
    noise: Vec<f32>,
    gains: Vec<f32>,
    re: [Vec<f32>; CH],
    im: [Vec<f32>; CH],
}

impl Denoise {
    fn new(rate: f32) -> Self {
        // √Hann (periodic): analysis × synthesis = Hann, which sums to 2 at a quarter-frame hop.
        let window = (0..NR_N).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f32 / NR_N as f32).cos()).sqrt()).collect();
        let bins = NR_N / 2 + 1;
        Denoise {
            rate,
            window,
            hist: [vec![0.0; NR_N], vec![0.0; NR_N]],
            acc: [vec![0.0; NR_N], vec![0.0; NR_N]],
            pending: 0,
            out: std::iter::repeat_n([0.0; 2], NR_HOP).collect(),
            power: vec![0.0; bins],
            noise: vec![f32::MAX; bins],
            gains: vec![1.0; bins],
            re: [vec![0.0; NR_N], vec![0.0; NR_N]],
            im: [vec![0.0; NR_N], vec![0.0; NR_N]],
        }
    }

    fn frame(&mut self, reduction_db: f32, sensitivity: f32) {
        let bins = NR_N / 2 + 1;
        for ch in 0..CH {
            for i in 0..NR_N {
                self.re[ch][i] = self.hist[ch][i] * self.window[i];
                self.im[ch][i] = 0.0;
            }
            let (re, im) = (&mut self.re[ch], &mut self.im[ch]);
            fft(re, im, false);
        }
        if reduction_db > 0.0 {
            let floor = gain(-reduction_db);
            // How far above the noise floor a bin must be to pass untouched.
            let over = 1.5 + 5.0 * sensitivity / 100.0;
            // The floor may rise 3 dB a second (it follows the quietest moments).
            let rise = 10f32.powf(3.0 / 10.0 * NR_HOP as f32 / self.rate);
            let mut g = vec![0.0f32; bins];
            for k in 0..bins {
                let p = (0..CH).map(|ch| self.re[ch][k].powi(2) + self.im[ch][k].powi(2)).sum::<f32>() / CH as f32;
                self.power[k] = if self.noise[k] == f32::MAX { p } else { 0.8 * self.power[k] + 0.2 * p };
                self.noise[k] = (self.noise[k] * rise).min(self.power[k].max(1e-12));
                // A running minimum sits below the noise's average: scale it back up.
                let want = (1.0 - over * NR_BIAS * self.noise[k] / self.power[k].max(1e-12)).max(floor);
                // Opens at once; closes more slowly (speech tails stay natural).
                self.gains[k] = if want > self.gains[k] { want } else { self.gains[k] + (want - self.gains[k]) * 0.35 };
            }
            // Smooth across frequency, so single bins don't flicker.
            for (k, gk) in g.iter_mut().enumerate() {
                let (a, b) = (self.gains[k.saturating_sub(1)], self.gains[(k + 1).min(bins - 1)]);
                *gk = 0.25 * a + 0.5 * self.gains[k] + 0.25 * b;
            }
            for ch in 0..CH {
                for k in 0..bins {
                    self.re[ch][k] *= g[k];
                    self.im[ch][k] *= g[k];
                    if k > 0 && k < NR_N / 2 {
                        // Keep the spectrum conjugate-symmetric (a real signal).
                        self.re[ch][NR_N - k] = self.re[ch][k];
                        self.im[ch][NR_N - k] = -self.im[ch][k];
                    }
                }
            }
        }
        for ch in 0..CH {
            let (re, im) = (&mut self.re[ch], &mut self.im[ch]);
            fft(re, im, true);
            // Inverse FFT (÷N), synthesis window, and ÷2 for the Hann overlap.
            let scale = 0.5 / NR_N as f32;
            for i in 0..NR_N {
                self.acc[ch][i] += self.re[ch][i] * self.window[i] * scale;
            }
        }
        for i in 0..NR_HOP {
            self.out.push_back([self.acc[0][i], self.acc[1][i]]);
        }
        for ch in 0..CH {
            self.acc[ch].copy_within(NR_HOP.., 0);
            let n = self.acc[ch].len();
            self.acc[ch][n - NR_HOP..].fill(0.0);
        }
    }
}

impl Proc for Denoise {
    fn process(&mut self, buf: &mut [f32], p: &[f32]) {
        for f in buf.chunks_exact_mut(CH) {
            for ch in 0..CH {
                self.hist[ch].copy_within(1.., 0);
                self.hist[ch][NR_N - 1] = f[ch];
            }
            self.pending += 1;
            if self.pending == NR_HOP {
                self.pending = 0;
                self.frame(p[denoise::REDUCTION], p[denoise::SENSITIVITY]);
            }
            let y = self.out.pop_front().unwrap_or([0.0; 2]);
            f[0] = y[0];
            f[1] = y[1];
        }
    }
    fn latency(&self) -> usize {
        NR_N
    }
    fn settle(&self) -> f64 {
        1.5
    }
}

// ---- The chain ------------------------------------------------------------------------

/// A clip's effects, in order, with their state.
pub struct FxChain {
    kinds: Vec<AudioFxKind>,
    procs: Vec<Box<dyn Proc>>,
    rate: u32,
}

impl FxChain {
    pub fn new(kinds: &[AudioFxKind], rate: u32) -> Self {
        let r = rate as f32;
        let procs = kinds
            .iter()
            .map(|k| -> Box<dyn Proc> {
                match k {
                    AudioFxKind::Eq => Box::new(Eq { rate: r, last: Vec::new(), cut: Biquad::default(), low: Biquad::default(), mid: Biquad::default(), high: Biquad::default() }),
                    AudioFxKind::Compressor => Box::new(Compressor { rate: r, gr: 0.0 }),
                    AudioFxKind::Limiter => Box::new(Limiter::new(r)),
                    AudioFxKind::Reverb => Box::new(Reverb::new(r)),
                    AudioFxKind::NoiseReduction => Box::new(Denoise::new(r)),
                }
            })
            .collect();
        FxChain { kinds: kinds.to_vec(), procs, rate }
    }

    /// The effects it was made for (a different list needs a new chain).
    pub fn kinds(&self) -> &[AudioFxKind] {
        &self.kinds
    }

    /// Start afresh (after a jump): empty delay lines and envelopes.
    pub fn reset(&mut self) {
        *self = FxChain::new(&self.kinds.clone(), self.rate);
    }

    /// Samples the output lags the input by, over the whole chain.
    pub fn latency(&self) -> usize {
        self.procs.iter().map(|p| p.latency()).sum()
    }

    /// Samples of sound before a jump it needs to settle.
    pub fn settle(&self) -> usize {
        (self.procs.iter().map(|p| p.settle()).fold(0.0, f64::max) * self.rate as f64) as usize
    }

    /// Run `buf` (interleaved stereo) through effect `i`… in order, effect `i`
    /// with parameter values `params[i]`.
    pub fn process(&mut self, buf: &mut [f32], params: &[Vec<f32>]) {
        for (p, v) in self.procs.iter_mut().zip(params) {
            p.process(buf, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::audiofx::AudioEffect;
    use reelforge_core::Time;

    const RATE: u32 = 48_000;

    fn params(kind: AudioFxKind, set: &[(usize, f32)]) -> Vec<f32> {
        let mut e = AudioEffect::new(kind);
        for &(i, v) in set {
            e.params[i].value = v;
        }
        let mut out = Vec::new();
        e.values_at(Time::ZERO, &mut out);
        out
    }

    fn sine(freq: f32, amp: f32, secs: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        (0..n).flat_map(|i| {
            let v = amp * (2.0 * PI * freq * i as f32 / RATE as f32).sin();
            [v, v]
        })
        .collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|v| v * v).sum::<f32>() / buf.len().max(1) as f32).sqrt()
    }

    /// Run through one effect in blocks of 480 frames; returns the output.
    fn run(kind: AudioFxKind, p: &[f32], input: &[f32]) -> Vec<f32> {
        let mut c = FxChain::new(&[kind], RATE);
        let mut out = input.to_vec();
        for b in out.chunks_mut(480 * 2) {
            c.process(b, &[p.to_vec()]);
        }
        out
    }

    #[test]
    fn eq_shapes_the_spectrum() {
        // +12 dB at 1 kHz on a 1 kHz tone: about four times the level.
        let tone = sine(1000.0, 0.1, 0.5);
        let out = run(AudioFxKind::Eq, &params(AudioFxKind::Eq, &[(eq::MID_GAIN, 12.0)]), &tone);
        let k = rms(&out[24_000..]) / rms(&tone[24_000..]);
        assert!((db(k) - 12.0).abs() < 0.3, "{:.2} dB", db(k));
        // The low cut at 200 Hz takes 50 Hz down a lot (12 dB/octave: ~24 dB at two octaves).
        let low = sine(50.0, 0.1, 1.0);
        let out = run(AudioFxKind::Eq, &params(AudioFxKind::Eq, &[(eq::LOW_CUT, 200.0)]), &low);
        let k = db(rms(&out[48_000..]) / rms(&low[48_000..]));
        assert!(k < -20.0, "{k:.1} dB");
        // A flat EQ passes the sound untouched.
        let out = run(AudioFxKind::Eq, &params(AudioFxKind::Eq, &[]), &tone);
        assert_eq!(out, tone);
    }

    #[test]
    fn compressor_turns_loud_passages_down() {
        // A tone at −6 dB (peaks at −3), threshold −20, 4:1, no knee: the
        // detector reads peaks, so it settles about 17·¾ ≈ 12.75 dB down at the
        // peaks, a little less on average.
        let p = params(AudioFxKind::Compressor, &[(comp::THRESHOLD, -20.0), (comp::RATIO, 4.0), (comp::KNEE, 0.0)]);
        let tone = sine(440.0, gain(-6.0) * std::f32::consts::SQRT_2, 1.0);
        let out = run(AudioFxKind::Compressor, &p, &tone);
        let k = db(rms(&out[72_000..]) / rms(&tone[72_000..]));
        assert!((-13.0..-11.0).contains(&k), "{k:.2} dB");
        // Below the threshold, untouched.
        let quiet = sine(440.0, 0.01, 0.5);
        let out = run(AudioFxKind::Compressor, &p, &quiet);
        assert!((rms(&out) / rms(&quiet) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn limiter_holds_the_ceiling_and_delays_by_its_look_ahead() {
        let p = params(AudioFxKind::Limiter, &[(limit::GAIN, 12.0), (limit::CEILING, -3.0)]);
        let tone = sine(200.0, 0.5, 1.0);
        let out = run(AudioFxKind::Limiter, &p, &tone);
        let ceil = gain(-3.0);
        assert!(out.iter().all(|v| v.abs() <= ceil + 1e-6));
        // It does reach the ceiling (it's limiting, not just quieter).
        assert!(out[48_000..].iter().fold(0.0f32, |m, v| m.max(v.abs())) > ceil * 0.95);
        // A quiet click comes out unchanged, 5 ms late.
        let mut click = vec![0.0f32; 4800];
        click[200] = 0.1;
        let out = run(AudioFxKind::Limiter, &params(AudioFxKind::Limiter, &[(limit::CEILING, 0.0)]), &click);
        let lat = FxChain::new(&[AudioFxKind::Limiter], RATE).latency();
        assert_eq!(lat, 240);
        assert!((out[200 + lat * 2] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn reverb_adds_a_tail() {
        let mut click = vec![0.0f32; 48_000 * 2];
        click[0] = 1.0;
        click[1] = 1.0;
        let out = run(AudioFxKind::Reverb, &params(AudioFxKind::Reverb, &[(reverb::MIX, 50.0)]), &click);
        // Sound a quarter and half a second later, fading.
        let (a, b) = (rms(&out[24_000..36_000]), rms(&out[60_000..72_000]));
        assert!(a > 1e-4 && b > 1e-6 && b < a, "{a} {b}");
        // Mix 0: dry only.
        let out = run(AudioFxKind::Reverb, &params(AudioFxKind::Reverb, &[(reverb::MIX, 0.0)]), &click);
        assert_eq!(out, click);
    }

    #[test]
    fn noise_reduction_passes_speech_like_tones_and_lowers_steady_noise() {
        // At 0 dB of reduction it's a pure delay of its latency.
        let tone = sine(300.0, 0.3, 0.5);
        let out = run(AudioFxKind::NoiseReduction, &params(AudioFxKind::NoiseReduction, &[(denoise::REDUCTION, 0.0)]), &tone);
        let lat = NR_N;
        for i in (lat * 2..tone.len()).step_by(97) {
            assert!((out[i] - tone[i - lat * 2]).abs() < 1e-4, "at {i}: {} vs {}", out[i], tone[i - lat * 2]);
        }
        // Hiss alone for 2 s, then hiss with a tone bursting in: the hiss is turned
        // down (by several dB), the tone kept (within a dB or so).
        let mut rng = 12345u32;
        let mut noise = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            (rng as f32 / u32::MAX as f32 - 0.5) * 0.02
        };
        let secs = 4.0;
        let n = (secs * RATE as f32) as usize;
        let mut hiss = vec![0.0f32; n * 2];
        let mut with = vec![0.0f32; n * 2];
        for i in 0..n {
            let h = noise();
            let t = if i > n / 2 { 0.2 * (2.0 * PI * 500.0 * i as f32 / RATE as f32).sin() } else { 0.0 };
            hiss[i * 2] = h;
            hiss[i * 2 + 1] = h;
            with[i * 2] = h + t;
            with[i * 2 + 1] = h + t;
        }
        let p = params(AudioFxKind::NoiseReduction, &[(denoise::REDUCTION, 20.0)]);
        let out = run(AudioFxKind::NoiseReduction, &p, &hiss);
        let k = db(rms(&out[n..n * 2 - 4096]) / rms(&hiss[n..n * 2 - 4096]));
        assert!(k < -8.0, "hiss only {k:.1} dB");
        let out = run(AudioFxKind::NoiseReduction, &p, &with);
        let tail = |b: &[f32]| rms(&b[(n * 3 / 2)..n * 2 - 4096]);
        let k = db(tail(&out) / tail(&with));
        assert!(k > -1.5, "tone kept: {k:.2} dB");
    }

    #[test]
    fn a_chain_adds_up_latency_and_settling() {
        let c = FxChain::new(&[AudioFxKind::Eq, AudioFxKind::Limiter, AudioFxKind::NoiseReduction], RATE);
        assert_eq!(c.latency(), 240 + NR_N);
        assert_eq!(c.settle(), 72_000);
        assert_eq!(c.kinds(), &[AudioFxKind::Eq, AudioFxKind::Limiter, AudioFxKind::NoiseReduction]);
    }
}
