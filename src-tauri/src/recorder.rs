//! A live recording session: captures audio, cuts it into chunks at quiet
//! moments, transcribes each chunk with OpenAI, and appends the resulting
//! segments straight into the meeting's markdown file.

use crate::audio::{self, Shared};
use crate::notes::{self, Segment};
use crate::openai::{self, AiConfig};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Phrases speech models tend to hallucinate on near-silent audio.
const HALLUCINATIONS: &[&str] = &[
    "thank you.",
    "thank you",
    "thanks for watching!",
    "thanks for watching.",
    "thank you for watching.",
    "you",
    "bye.",
    "so",
    "okay.",
    "subtitles by the amara.org community",
];

pub struct Session {
    pub meeting_id: String,
    pub started: Instant,
    pub offset_secs: f64,
    pub mic: bool,
    pub system: bool,
    stop_capture: mpsc::Sender<()>,
    capture_thread: Option<std::thread::JoinHandle<()>>,
    stop_chunker: tokio::sync::watch::Sender<bool>,
    chunker: tauri::async_runtime::JoinHandle<()>,
    ctx: Arc<Ctx>,
}

#[derive(Serialize, Clone)]
pub struct Status {
    pub meeting_id: String,
    pub elapsed_secs: f64,
    pub offset_secs: f64,
    pub mic: bool,
    pub system: bool,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone)]
struct Levels {
    me: f32,
    them: f32,
}

#[derive(Serialize, Clone)]
struct SegmentEvent {
    meeting_id: String,
    segment: Segment,
}

#[derive(Serialize, Clone)]
struct ErrorEvent {
    meeting_id: String,
    message: String,
}

struct Ctx {
    app: AppHandle,
    http: reqwest::Client,
    ai: AiConfig,
    notes_dir: PathBuf,
    file_lock: Arc<Mutex<()>>,
    meeting_id: String,
    meeting_title: String,
    offset_secs: f64,
    started: Instant,
    chunk_secs: u32,
    last_text: Mutex<HashMap<String, String>>,
    pending: Mutex<usize>,
}

struct Source {
    is_mic: bool,
    speaker: String,
    buf: Shared,
    /// Native-rate samples already handed off for transcription.
    consumed: u64,
}

pub struct StartArgs {
    pub app: AppHandle,
    pub http: reqwest::Client,
    pub ai: AiConfig,
    pub notes_dir: PathBuf,
    pub file_lock: Arc<Mutex<()>>,
    pub meeting_id: String,
    pub mic_device: String,
    pub capture_system: bool,
    pub my_name: String,
    pub their_name: String,
    pub chunk_secs: u32,
}

impl Session {
    pub fn start(a: StartArgs) -> Result<(Session, Vec<String>)> {
        let meeting = {
            let _g = a.file_lock.lock().unwrap();
            notes::load(&a.notes_dir, &a.meeting_id)?
        };
        // Resume after any earlier recording so timestamps keep increasing.
        let offset_secs = meeting
            .transcript
            .last()
            .map(|s| s.start + 1.0)
            .unwrap_or(0.0)
            .max(meeting.duration_secs as f64);

        let mic = audio::shared();
        let sys = audio::shared();
        let (stop_capture, thread, info) =
            audio::spawn_capture(a.mic_device.clone(), a.capture_system, mic.clone(), sys.clone())?;
        let started = Instant::now();

        let ctx = Arc::new(Ctx {
            app: a.app,
            http: a.http,
            ai: a.ai,
            notes_dir: a.notes_dir,
            file_lock: a.file_lock,
            meeting_id: a.meeting_id.clone(),
            meeting_title: meeting.title.clone(),
            offset_secs,
            started,
            chunk_secs: a.chunk_secs,
            last_text: Mutex::new(HashMap::new()),
            pending: Mutex::new(0),
        });

        let mut sources = Vec::new();
        if info.mic {
            sources.push(Source { is_mic: true, speaker: a.my_name, buf: mic, consumed: 0 });
        }
        if info.system {
            sources.push(Source { is_mic: false, speaker: a.their_name, buf: sys, consumed: 0 });
        }

        let (stop_chunker, stop_rx) = tokio::sync::watch::channel(false);
        let chunker = tauri::async_runtime::spawn(run_chunker(ctx.clone(), sources, stop_rx));

        Ok((
            Session {
                meeting_id: a.meeting_id,
                started,
                offset_secs,
                mic: info.mic,
                system: info.system,
                stop_capture,
                capture_thread: Some(thread),
                stop_chunker,
                chunker,
                ctx,
            },
            info.warnings,
        ))
    }

    pub fn status(&self, warnings: Vec<String>) -> Status {
        Status {
            meeting_id: self.meeting_id.clone(),
            elapsed_secs: self.started.elapsed().as_secs_f64(),
            offset_secs: self.offset_secs,
            mic: self.mic,
            system: self.system,
            warnings,
        }
    }

    /// Stops capture, transcribes whatever audio is left, waits for in-flight
    /// requests, and records the meeting duration.
    pub async fn stop(mut self) -> Result<notes::Meeting> {
        let _ = self.stop_capture.send(());
        if let Some(t) = self.capture_thread.take() {
            let _ = tauri::async_runtime::spawn_blocking(move || t.join()).await;
        }
        let _ = self.stop_chunker.send(true);
        let _ = (&mut self.chunker).await;

        let ctx = &self.ctx;
        let _g = ctx.file_lock.lock().unwrap();
        let mut m = notes::load(&ctx.notes_dir, &ctx.meeting_id)?;
        let end = ctx.offset_secs + self.started.elapsed().as_secs_f64();
        m.duration_secs = m.duration_secs.max(end.round() as u64);
        notes::save(&ctx.notes_dir, &mut m)?;
        Ok(m)
    }
}

async fn run_chunker(ctx: Arc<Ctx>, mut sources: Vec<Source>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut tasks = Vec::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    let mut tick: u64 = 0;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.changed() => break,
        }
        tick += 1;
        emit_levels(&ctx, &sources);
        if tick.is_multiple_of(10) {
            for src in sources.iter_mut() {
                if let Some(job) = take_chunk(&ctx, src, false) {
                    tasks.push(tauri::async_runtime::spawn(transcribe_job(ctx.clone(), job)));
                }
            }
        }
    }
    for src in sources.iter_mut() {
        if let Some(job) = take_chunk(&ctx, src, true) {
            tasks.push(tauri::async_runtime::spawn(transcribe_job(ctx.clone(), job)));
        }
    }
    for t in tasks {
        let _ = t.await;
    }
}

fn emit_levels(ctx: &Ctx, sources: &[Source]) {
    let mut levels = Levels { me: 0.0, them: 0.0 };
    for src in sources {
        if let Ok(mut b) = src.buf.lock() {
            let l = std::mem::take(&mut b.level);
            if src.is_mic {
                levels.me = l;
            } else {
                levels.them = l;
            }
        }
    }
    let _ = ctx.app.emit("audio-level", levels);
}

struct Job {
    speaker: String,
    start: f64,
    samples: Vec<f32>,
    rate: u32,
}

fn take_chunk(ctx: &Ctx, src: &mut Source, final_flush: bool) -> Option<Job> {
    let mut b = src.buf.lock().ok()?;
    let rate = b.rate;
    if rate == 0 {
        return None;
    }
    // WASAPI loopback delivers nothing while the computer is silent; pad with
    // silence so chunk timestamps stay aligned with the wall clock.
    let expected = (ctx.started.elapsed().as_secs_f64() * rate as f64) as u64;
    if expected > b.total + rate as u64 / 2 {
        let gap = (expected - b.total) as usize;
        b.samples.extend(std::iter::repeat_n(0.0, gap));
        b.total += gap as u64;
    }

    let target = ctx.chunk_secs as usize * rate as usize;
    let len = b.samples.len();
    if final_flush {
        if len < rate as usize / 2 {
            return None;
        }
    } else if len < target {
        return None;
    }
    let cut = if final_flush { len } else { audio::find_cut(&b.samples, rate, target) };
    let samples: Vec<f32> = b.samples.drain(..cut).collect();
    drop(b);
    let start = ctx.offset_secs + src.consumed as f64 / rate as f64;
    src.consumed += samples.len() as u64;
    Some(Job { speaker: src.speaker.clone(), start, samples, rate })
}

fn set_pending(ctx: &Ctx, delta: isize) {
    let n = {
        let mut p = ctx.pending.lock().unwrap();
        *p = (*p as isize + delta).max(0) as usize;
        *p
    };
    let _ = ctx.app.emit("transcription-pending", n);
}

async fn transcribe_job(ctx: Arc<Ctx>, job: Job) {
    let mut samples = audio::resample(&job.samples, job.rate);
    let voiced = audio::voiced_seconds(&samples, audio::TARGET_RATE);
    if voiced < 0.4 {
        return;
    }
    audio::normalize(&mut samples);
    let wav = audio::encode_wav(&samples, audio::TARGET_RATE);

    let prompt = {
        let last = ctx.last_text.lock().unwrap();
        let prev = last.get(&job.speaker).cloned().unwrap_or_default();
        let tail: String = prev.chars().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
        if tail.is_empty() {
            format!("Meeting: {}.", ctx.meeting_title)
        } else {
            tail
        }
    };

    set_pending(&ctx, 1);
    let result = openai::transcribe(&ctx.http, &ctx.ai, wav, &prompt).await;
    set_pending(&ctx, -1);

    let text = match result {
        Ok(t) => t,
        Err(e) => {
            let _ = ctx.app.emit(
                "recording-error",
                ErrorEvent { meeting_id: ctx.meeting_id.clone(), message: format!("Transcription failed: {e:#}") },
            );
            return;
        }
    };
    let text = text.trim().to_string();
    if text.is_empty() || (voiced < 2.0 && HALLUCINATIONS.contains(&text.to_lowercase().as_str())) {
        return;
    }
    ctx.last_text.lock().unwrap().insert(job.speaker.clone(), text.clone());

    let segment = Segment { start: (job.start * 10.0).round() / 10.0, speaker: job.speaker, text };
    let saved = {
        let _g = ctx.file_lock.lock().unwrap();
        notes::load(&ctx.notes_dir, &ctx.meeting_id).and_then(|mut m| {
            notes::insert_segment(&mut m, segment.clone());
            notes::save(&ctx.notes_dir, &mut m)
        })
    };
    if let Err(e) = saved {
        let _ = ctx.app.emit(
            "recording-error",
            ErrorEvent { meeting_id: ctx.meeting_id.clone(), message: format!("Couldn't save transcript: {e:#}") },
        );
    }
    let _ = ctx.app.emit("transcript-segment", SegmentEvent { meeting_id: ctx.meeting_id.clone(), segment });
}
