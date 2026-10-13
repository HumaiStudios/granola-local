//! Audio capture and the small amount of DSP needed before transcription.
//!
//! Two independent sources are captured so the transcript can tell who's who
//! without diarization:
//! * the microphone  → the user ("Me")
//! * system output via WASAPI loopback → everyone else on the call ("Them")
//!
//! cpal on Windows opens an *output* device in loopback mode when you build an
//! input stream on it, which is exactly what we need.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::{mpsc, Arc, Mutex};

pub const TARGET_RATE: u32 = 16_000;

/// Mono samples at the device's native rate, shared with the audio callback.
#[derive(Default)]
pub struct SourceBuffer {
    pub samples: Vec<f32>,
    pub rate: u32,
    /// Samples ever pushed (including ones already drained).
    pub total: u64,
    /// Peak RMS since the UI last read it.
    pub level: f32,
}

pub type Shared = Arc<Mutex<SourceBuffer>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(SourceBuffer::default()))
}

fn build_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, buf: Shared, label: &'static str) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let Ok(mut b) = buf.lock() else { return };
            let mut sumsq = 0.0f32;
            let mut n = 0usize;
            for frame in data.chunks(channels) {
                let s = frame.iter().map(|&x| f32::from_sample_(x)).sum::<f32>() / frame.len() as f32;
                b.samples.push(s);
                sumsq += s * s;
                n += 1;
            }
            if n > 0 {
                b.total += n as u64;
                let rms = (sumsq / n as f32).sqrt();
                if rms > b.level {
                    b.level = rms;
                }
            }
        },
        move |err| eprintln!("[audio:{label}] stream error: {err}"),
        None,
    )?;
    Ok(stream)
}

fn open(device: &cpal::Device, config: cpal::SupportedStreamConfig, buf: &Shared, label: &'static str) -> Result<cpal::Stream> {
    buf.lock().map_err(|_| anyhow!("audio buffer poisoned"))?.rate = config.sample_rate().0;
    let fmt = config.sample_format();
    let cfg: cpal::StreamConfig = config.into();
    let b = buf.clone();
    let stream = match fmt {
        cpal::SampleFormat::F32 => build_stream::<f32>(device, &cfg, b, label),
        cpal::SampleFormat::I16 => build_stream::<i16>(device, &cfg, b, label),
        cpal::SampleFormat::U16 => build_stream::<u16>(device, &cfg, b, label),
        cpal::SampleFormat::I32 => build_stream::<i32>(device, &cfg, b, label),
        cpal::SampleFormat::I8 => build_stream::<i8>(device, &cfg, b, label),
        cpal::SampleFormat::U8 => build_stream::<u8>(device, &cfg, b, label),
        cpal::SampleFormat::F64 => build_stream::<f64>(device, &cfg, b, label),
        other => Err(anyhow!("unsupported sample format {other:?}")),
    }?;
    stream.play().context("starting audio stream")?;
    Ok(stream)
}

pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

fn pick_mic(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if !name.trim().is_empty() {
        if let Ok(mut devs) = host.input_devices() {
            if let Some(d) = devs.find(|d| d.name().map(|n| n == name).unwrap_or(false)) {
                return Some(d);
            }
        }
    }
    host.default_input_device()
}

pub struct CaptureInfo {
    pub mic: bool,
    pub system: bool,
    pub warnings: Vec<String>,
}

/// Runs the cpal streams on a dedicated thread (streams aren't `Send` on every
/// platform). Returns once streams are up; dropping/sending on `stop` ends them.
pub fn spawn_capture(
    mic_device: String,
    capture_system: bool,
    mic: Shared,
    system: Shared,
) -> Result<(mpsc::Sender<()>, std::thread::JoinHandle<()>, CaptureInfo)> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<CaptureInfo, String>>();
    let handle = std::thread::Builder::new()
        .name("audio-capture".into())
        .spawn(move || {
            let host = cpal::default_host();
            let mut warnings = Vec::new();
            let mut streams = Vec::new();

            let mic_stream = pick_mic(&host, &mic_device)
                .ok_or_else(|| anyhow!("No microphone found"))
                .and_then(|d| {
                    let cfg = d.default_input_config().context("reading microphone format")?;
                    open(&d, cfg, &mic, "mic")
                });
            let mic_ok = match mic_stream {
                Ok(s) => {
                    streams.push(s);
                    true
                }
                Err(e) => {
                    warnings.push(format!("Microphone unavailable: {e:#}"));
                    false
                }
            };

            let mut sys_ok = false;
            if capture_system {
                if cfg!(windows) {
                    let res = host
                        .default_output_device()
                        .ok_or_else(|| anyhow!("No output device found"))
                        .and_then(|d| {
                            let cfg = d.default_output_config().context("reading output format")?;
                            open(&d, cfg, &system, "system")
                        });
                    match res {
                        Ok(s) => {
                            streams.push(s);
                            sys_ok = true;
                        }
                        Err(e) => warnings.push(format!("System audio unavailable: {e:#}")),
                    }
                } else {
                    warnings.push("System-audio capture needs Windows (WASAPI loopback); recording microphone only.".into());
                }
            }

            if !mic_ok && !sys_ok {
                let _ = ready_tx.send(Err(warnings.join(" ")));
                return;
            }
            let _ = ready_tx.send(Ok(CaptureInfo { mic: mic_ok, system: sys_ok, warnings }));
            // Park until told to stop (or the sender is dropped).
            let _ = stop_rx.recv();
            drop(streams);
        })
        .context("spawning audio thread")?;

    match ready_rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(info)) => Ok((stop_tx, handle, info)),
        Ok(Err(msg)) => Err(anyhow!(msg)),
        Err(_) => Err(anyhow!("Timed out opening audio devices")),
    }
}

// ---------- DSP ----------

/// Downsample (or upsample) to `TARGET_RATE` with box-filter averaging, which
/// doubles as a cheap anti-aliasing low-pass for speech.
pub fn resample(input: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == TARGET_RATE || input.is_empty() || from_rate == 0 {
        return input.to_vec();
    }
    let ratio = from_rate as f64 / TARGET_RATE as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let start = i as f64 * ratio;
        let end = start + ratio;
        if ratio > 1.0 {
            let a = start.floor() as usize;
            let b = (end.ceil() as usize).min(input.len()).max(a + 1);
            let slice = &input[a..b];
            out.push(slice.iter().sum::<f32>() / slice.len() as f32);
        } else {
            let a = start.floor() as usize;
            let frac = (start - a as f64) as f32;
            let x0 = input[a.min(input.len() - 1)];
            let x1 = input[(a + 1).min(input.len() - 1)];
            out.push(x0 + (x1 - x0) * frac);
        }
    }
    out
}

fn rms(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

/// Seconds of audio whose 30 ms frames exceed a speech-ish energy floor.
pub fn voiced_seconds(samples: &[f32], rate: u32) -> f32 {
    let frame = (rate as usize * 30 / 1000).max(1);
    let voiced = samples.chunks(frame).filter(|f| rms(f) > 0.006).count();
    voiced as f32 * 0.030
}

/// Where to cut a buffer of at least `min_len` samples: the quietest 100 ms
/// frame in the last ~5 s, so words aren't split across chunks.
pub fn find_cut(samples: &[f32], rate: u32, min_len: usize) -> usize {
    let len = samples.len();
    let frame = (rate as usize / 10).max(1);
    let window = (rate as usize * 5).min(len / 3);
    let search_start = len.saturating_sub(window).max(min_len.saturating_sub(window));
    let mut best = len;
    let mut best_energy = f32::MAX;
    let mut pos = search_start;
    while pos + frame <= len {
        let e = rms(&samples[pos..pos + frame]);
        if e < best_energy {
            best_energy = e;
            best = pos + frame / 2;
        }
        pos += frame;
    }
    best.clamp(1, len)
}

/// Gentle peak normalisation for quiet microphones (max 10x gain).
pub fn normalize(samples: &mut [f32]) {
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 0.0 && peak < 0.5 {
        let gain = (0.9 / peak).min(10.0);
        samples.iter_mut().for_each(|x| *x *= gain);
    }
}

/// 16-bit PCM mono WAV.
pub fn encode_wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_lengths() {
        assert_eq!(resample(&vec![0.0; 48_000], 48_000).len(), 16_000);
        assert_eq!(resample(&vec![0.0; 44_100], 44_100).len(), 16_000);
        assert_eq!(resample(&vec![0.0; 8_000], 8_000).len(), 16_000);
        assert_eq!(resample(&vec![0.5; 16_000], 16_000), vec![0.5; 16_000]);
    }

    #[test]
    fn resample_preserves_dc() {
        let out = resample(&vec![0.25; 4800], 48_000);
        assert!(out.iter().all(|x| (x - 0.25).abs() < 1e-6));
    }

    #[test]
    fn cut_lands_in_silence() {
        let rate = 16_000;
        // 20 s of "speech" with a 300 ms gap at 18 s.
        let mut s: Vec<f32> = (0..rate * 20).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
        let gap = (rate as f32 * 18.0) as usize;
        for x in &mut s[gap..gap + rate as usize * 3 / 10] {
            *x = 0.0;
        }
        let cut = find_cut(&s, rate, rate as usize * 20);
        assert!(cut >= gap && cut <= gap + rate as usize * 3 / 10, "cut {cut} not in gap {gap}");
    }

    #[test]
    fn wav_header() {
        let w = encode_wav(&[0.0, 1.0, -1.0], 16_000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(i16::from_le_bytes([w[46], w[47]]), i16::MAX);
    }

    #[test]
    fn voiced_detection() {
        assert_eq!(voiced_seconds(&vec![0.0; 16_000], 16_000), 0.0);
        assert!(voiced_seconds(&vec![0.1; 16_000], 16_000) > 0.9);
    }
}
