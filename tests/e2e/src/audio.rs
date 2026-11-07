//! Test signal generation and continuity analysis (no audio ever enters CallXL's Rust
//! code — these files are read by the C++ test ADM and analysed after the call).

use std::path::Path;

pub const RATE: u32 = 48_000;

/// One synthetic "talker": a steady tone plus a speech-like voiced signal (harmonic
/// series on a gliding pitch, shaped by moving formants, syllable-modulated but never
/// silent, so any gap in the far-end recording is a real dropout), plus a little noise.
/// Each talker has its own pitch range, syllable rate and formant motion so that two
/// peers' signals are uncorrelated — otherwise AEC3 rightly treats the near end as an
/// echo of the far end and suppresses it. 10 s, 48 kHz mono PCM16.
#[derive(Clone, Copy, Debug)]
pub struct Talker {
    pub tone_hz: f64,
    pub f0_hz: f64,
    pub syllable_hz: f64,
    pub formant_hz: f64,
    pub seed: u64,
}

pub const TALKERS: [Talker; 3] = [
    Talker { tone_hz: 440.0, f0_hz: 125.0, syllable_hz: 3.3, formant_hz: 0.9, seed: 1 },
    Talker { tone_hz: 660.0, f0_hz: 215.0, syllable_hz: 4.7, formant_hz: 0.61, seed: 2 },
    Talker { tone_hz: 550.0, f0_hz: 170.0, syllable_hz: 5.9, formant_hz: 0.43, seed: 3 },
];

pub fn tone_speech(t: Talker) -> Vec<i16> {
    use rand::{Rng, SeedableRng};
    use std::f64::consts::PI;
    let mut rng = rand::rngs::StdRng::seed_from_u64(t.seed);
    let n = (RATE * 10) as usize;
    let mut out = Vec::with_capacity(n);
    let mut phase_f0 = 0.0f64;
    for i in 0..n {
        let x = i as f64 / f64::from(RATE);
        let tone = 0.18 * (2.0 * PI * t.tone_hz * x).sin();
        let f0 = t.f0_hz * (1.0 + 0.25 * (2.0 * PI * 0.37 * t.syllable_hz / 4.0 * x).sin());
        phase_f0 += 2.0 * PI * f0 / f64::from(RATE);
        let f1 = 500.0 + 300.0 * (2.0 * PI * t.formant_hz * x).sin();
        let f2 = 1500.0 + 700.0 * (2.0 * PI * t.formant_hz * 0.6 * x + 1.0).sin();
        let mut voiced = 0.0;
        for h in 1..=60 {
            let fh = f0 * h as f64;
            if fh > 16_000.0 {
                break;
            }
            let g = (-((fh - f1) / 250.0).powi(2)).exp() + 0.6 * (-((fh - f2) / 400.0).powi(2)).exp() + 0.02;
            voiced += g * (phase_f0 * h as f64).sin();
        }
        let syllable = 0.55 + 0.45 * (2.0 * PI * t.syllable_hz * x).sin().abs();
        let noise: f64 = rng.gen_range(-1.0..1.0) * 0.02;
        let v = tone + 0.22 * syllable * voiced + noise;
        out.push((v.clamp(-1.0, 1.0) * 32_000.0) as i16);
    }
    out
}

pub fn write_wav(path: &Path, samples: &[i16]) -> anyhow::Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample(*s)?;
    }
    w.finalize()?;
    Ok(())
}

pub fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let mut r = hound::WavReader::open(path)?;
    Ok(r.samples::<i16>().filter_map(Result::ok).collect())
}

#[derive(Debug, Clone)]
pub struct Continuity {
    /// Length of the analysed (active) part, seconds.
    pub active_secs: f64,
    /// Longest run of near-silent 5 ms windows inside the active part, ms.
    pub longest_gap_ms: f64,
    /// Number of gaps longer than 20 ms.
    pub gaps_over_20ms: usize,
    pub median_rms: f64,
}

/// RMS in 5 ms windows; a window is "silent" below 10 % (−20 dB) of the median active
/// RMS. The active part starts 1 s after the first loud window (jitter-buffer warm-up)
/// and ends 0.5 s before the last one. `skip` lists (start_s, end_s) spans to exclude.
pub fn continuity(samples: &[i16]) -> Continuity {
    const WIN: usize = 240; // 5 ms
    let rms: Vec<f64> = samples
        .chunks(WIN)
        .map(|c| (c.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    let loud = 500.0;
    let first = rms.iter().position(|r| *r > loud);
    let last = rms.iter().rposition(|r| *r > loud);
    let (Some(first), Some(last)) = (first, last) else {
        return Continuity { active_secs: 0.0, longest_gap_ms: f64::INFINITY, gaps_over_20ms: 1, median_rms: 0.0 };
    };
    let start = first + 200;
    let end = last.saturating_sub(100);
    if end <= start {
        return Continuity { active_secs: 0.0, longest_gap_ms: f64::INFINITY, gaps_over_20ms: 1, median_rms: 0.0 };
    }
    let active = &rms[start..end];
    let mut sorted = active.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];
    let thr = median * 0.1;
    let (mut run, mut longest, mut gaps) = (0usize, 0usize, 0usize);
    for r in active {
        if *r < thr {
            run += 1;
        } else {
            if run * 5 > 20 {
                gaps += 1;
            }
            longest = longest.max(run);
            run = 0;
        }
    }
    if run * 5 > 20 {
        gaps += 1;
    }
    longest = longest.max(run);
    Continuity {
        active_secs: active.len() as f64 * 0.005,
        longest_gap_ms: longest as f64 * 5.0,
        gaps_over_20ms: gaps,
        median_rms: median,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_signal_is_continuous_and_gap_detector_works() {
        let s = tone_speech(TALKERS[0]);
        let c = continuity(&s);
        assert_eq!(c.gaps_over_20ms, 0, "{c:?}");
        let mut holed = s.clone();
        for x in &mut holed[96_000..96_000 + 1_920] {
            *x = 0; // 40 ms hole
        }
        let c = continuity(&holed);
        assert_eq!(c.gaps_over_20ms, 1, "{c:?}");
        assert!(c.longest_gap_ms >= 35.0);
    }
}
