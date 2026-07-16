//! Local voice dictation: microphone capture (`cpal`) + local Whisper transcription
//! (`whisper-rs`, in-process bindings over whisper.cpp — see [`crate::voice_models`] for why
//! this isn't a downloaded subprocess like [`crate::local_models`]).
//!
//! [`VoiceManager`] is cheap to clone; every clone talks to the same dedicated background
//! thread (mirrors [`crate::compute::TunnelManager`]). The thread is plain `std::thread`, not
//! Tokio, because both `cpal` streams and `whisper-rs` inference are synchronous/blocking and
//! a `cpal::Stream` is not `Send` — it has to be created and dropped on the same thread.
//!
//! The whisper model is loaded lazily: nothing is loaded at `spawn()` time, only on the first
//! [`VoiceCmd::StopAndTranscribe`]. When `keep_loaded` is false (the default), the model is
//! dropped again immediately after each transcription so idle dictation costs no memory.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const TARGET_SAMPLE_RATE: u32 = 16_000;

pub enum VoiceCmd {
    StartRecording,
    StopAndTranscribe {
        model_path: PathBuf,
        keep_loaded: bool,
        language: String,
    },
    Unload,
}

#[derive(Debug, Clone)]
pub enum VoiceMsg {
    RecordingStarted(Result<(), String>),
    ModelLoading,
    TranscriptionDone(Result<String, String>),
}

/// Cheap to clone; every clone talks to the same background voice-engine thread.
#[derive(Clone)]
pub struct VoiceManager {
    tx: mpsc::Sender<VoiceCmd>,
}

impl VoiceManager {
    /// Spawn the dedicated background thread. Call once at app startup; the returned handle
    /// and receiver are safe to share/move to the UI thread.
    pub fn spawn() -> (Self, mpsc::Receiver<VoiceMsg>) {
        let (tx, rx) = mpsc::channel::<VoiceCmd>();
        let (result_tx, result_rx) = mpsc::channel::<VoiceMsg>();
        std::thread::spawn(move || run(rx, result_tx));
        (Self { tx }, result_rx)
    }

    pub fn start_recording(&self) {
        let _ = self.tx.send(VoiceCmd::StartRecording);
    }

    pub fn stop_and_transcribe(&self, model_path: PathBuf, keep_loaded: bool, language: String) {
        let _ = self.tx.send(VoiceCmd::StopAndTranscribe {
            model_path,
            keep_loaded,
            language,
        });
    }

    pub fn unload(&self) {
        let _ = self.tx.send(VoiceCmd::Unload);
    }
}

struct ActiveRecording {
    stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    channels: usize,
    sample_rate: u32,
}

fn run(rx: mpsc::Receiver<VoiceCmd>, result_tx: mpsc::Sender<VoiceMsg>) {
    let mut recording: Option<ActiveRecording> = None;
    let mut loaded: Option<(PathBuf, WhisperContext)> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            VoiceCmd::StartRecording => {
                let result = start_recording();
                match result {
                    Ok(active) => {
                        recording = Some(active);
                        let _ = result_tx.send(VoiceMsg::RecordingStarted(Ok(())));
                    }
                    Err(e) => {
                        let _ = result_tx.send(VoiceMsg::RecordingStarted(Err(e)));
                    }
                }
            }
            VoiceCmd::StopAndTranscribe {
                model_path,
                keep_loaded,
                language,
            } => {
                let Some(active) = recording.take() else {
                    let _ = result_tx.send(VoiceMsg::TranscriptionDone(Err(
                        "not recording".to_string()
                    )));
                    continue;
                };
                drop(active.stream);
                let samples = resample_mono_16k(
                    &active.samples.lock().unwrap_or_else(|e| e.into_inner()),
                    active.channels,
                    active.sample_rate,
                );

                if loaded.as_ref().map(|(p, _)| p) != Some(&model_path) {
                    loaded = None;
                    let _ = result_tx.send(VoiceMsg::ModelLoading);
                    match WhisperContext::new_with_params(
                        &model_path,
                        WhisperContextParameters::default(),
                    ) {
                        Ok(ctx) => loaded = Some((model_path.clone(), ctx)),
                        Err(e) => {
                            let _ = result_tx.send(VoiceMsg::TranscriptionDone(Err(format!(
                                "failed to load voice model: {e}"
                            ))));
                            continue;
                        }
                    }
                }

                let Some((_, ctx)) = loaded.as_ref() else {
                    continue;
                };
                let text = transcribe(ctx, &samples, &language);
                let _ = result_tx.send(VoiceMsg::TranscriptionDone(text));

                if !keep_loaded {
                    loaded = None;
                }
            }
            VoiceCmd::Unload => {
                loaded = None;
            }
        }
    }
}

fn start_recording() -> Result<ActiveRecording, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No microphone input device found")?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("No usable microphone input config: {e}"))?;
    let sample_format = supported.sample_format();
    let channels = supported.channels() as usize;
    let sample_rate = supported.sample_rate().0;
    let config: cpal::StreamConfig = supported.into();

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let err_fn = |err| eprintln!("oxi: microphone stream error: {err}");

    let stream = {
        let samples = samples.clone();
        match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    if let Ok(mut buf) = samples.lock() {
                        buf.extend_from_slice(data);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    if let Ok(mut buf) = samples.lock() {
                        buf.extend(data.iter().map(|&s| s as f32 / i16::MAX as f32));
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    if let Ok(mut buf) = samples.lock() {
                        buf.extend(data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0));
                    }
                },
                err_fn,
                None,
            ),
            other => return Err(format!("Unsupported microphone sample format: {other:?}")),
        }
        .map_err(|e| format!("Could not open microphone stream: {e}"))?
    };
    stream
        .play()
        .map_err(|e| format!("Could not start microphone stream: {e}"))?;

    Ok(ActiveRecording {
        stream,
        samples,
        channels,
        sample_rate,
    })
}

fn transcribe(ctx: &WhisperContext, samples: &[f32], language: &str) -> Result<String, String> {
    let mut state = ctx.create_state().map_err(|e| e.to_string())?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_progress(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    let lang = language.trim();
    if !lang.is_empty() && lang != "auto" {
        params.set_language(Some(lang));
    }
    state.full(params, samples).map_err(|e| e.to_string())?;
    let mut out = String::new();
    for segment in state.as_iter() {
        if let Ok(text) = segment.to_str() {
            out.push_str(text.trim());
            out.push(' ');
        }
    }
    Ok(out.trim().to_string())
}

/// Downmix to mono and linearly resample to the 16kHz whisper.cpp expects. Speech dictation
/// doesn't need a high-quality resampler; linear interpolation is more than good enough and
/// avoids pulling in another crate.
fn resample_mono_16k(samples: &[f32], channels: usize, sample_rate: u32) -> Vec<f32> {
    let mono: Vec<f32> = if channels <= 1 {
        samples.to_vec()
    } else {
        samples
            .chunks(channels)
            .map(|c| c.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    if sample_rate == TARGET_SAMPLE_RATE || mono.is_empty() {
        return mono;
    }
    let ratio = sample_rate as f64 / TARGET_SAMPLE_RATE as f64;
    let out_len = (mono.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = mono.get(idx).copied().unwrap_or(0.0);
        let b = mono.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}
