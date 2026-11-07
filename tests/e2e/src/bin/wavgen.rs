//! Writes the e2e test signal: `callxl-wavgen <out.wav> [talker 0..2]`.
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "tone_speech_48k_mono.wav".into());
    let i: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(0).min(2);
    callxl_e2e::audio::write_wav(
        std::path::Path::new(&out),
        &callxl_e2e::audio::tone_speech(callxl_e2e::audio::TALKERS[i]),
    )?;
    println!("{out}");
    Ok(())
}
