//! Local whisper.cpp transcription backend via `whisper-rs`.
//!
//! Uses whisper-rs with a sliding window approach for pseudo-streaming.
//!
//! Live typing mode: each window is deduplicated against committed text and
//! only novel suffixes are emitted. Review/overlay mode: windows are sent as
//! live preview only; the daemon runs one full-audio transcription for paste.

use std::sync::Arc;

use asr_dedup::{dedup_window_text, prompt_tail};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::{TranscriptionBackend, TranscriptionConfig};
use crate::audio::AudioChunk;

/// Sliding window parameters for pseudo-streaming.
const WINDOW_SECS: usize = 5;
const STEP_SECS: usize = 1;
const SAMPLE_RATE: usize = 16_000;
const WINDOW_SAMPLES: usize = WINDOW_SECS * SAMPLE_RATE;
const STEP_SAMPLES: usize = STEP_SECS * SAMPLE_RATE;
/// Shorter initial window for faster first result.
const INITIAL_WINDOW_SECS: usize = 2;
const INITIAL_WINDOW_SAMPLES: usize = INITIAL_WINDOW_SECS * SAMPLE_RATE;

/// Silence threshold for skipping inference windows — slightly stricter than the
/// daemon auto-stop threshold so we do not send borderline noise to whisper.
const INFERENCE_SILENCE_THRESHOLD: f64 = 0.006;

/// Segments with higher no_speech probability are discarded as non-speech.
const NO_SPEECH_SEGMENT_THRESHOLD: f32 = 0.50;

/// Maximum words passed as whisper `initial_prompt` (full committed text causes
/// echo after silence gaps).
const PROMPT_TAIL_WORDS: usize = 20;

/// Skip `initial_prompt` after this many consecutive silent inference windows
/// (~1 s each) so whisper does not regurgitate stale context after a pause.
const SILENT_SKIPS_BEFORE_OMIT_PROMPT: u32 = 2;

struct StreamInferJob {
    samples: Vec<f32>,
    language: String,
    prompt: Option<String>,
    result_tx: oneshot::Sender<anyhow::Result<String>>,
}

/// Inference worker for one recording session. Whisper state must not be
/// reused across sessions or the previous transcript can bleed into the next.
fn start_stream_infer_thread(
    ctx: Arc<whisper_rs::WhisperContext>,
) -> anyhow::Result<std::sync::mpsc::SyncSender<StreamInferJob>> {
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<StreamInferJob>(1);

    std::thread::Builder::new()
        .name("whisper-stream".into())
        .spawn(move || {
            let mut state = match ctx.create_state() {
                Ok(state) => state,
                Err(e) => {
                    let err = format!("failed to create whisper streaming state: {e}");
                    while let Ok(job) = job_rx.recv() {
                        let _ = job.result_tx.send(Err(anyhow::anyhow!("{err}")));
                    }
                    return;
                }
            };

            while let Ok(job) = job_rx.recv() {
                let result = run_whisper_inference_on_state(
                    &mut state,
                    &job.samples,
                    &job.language,
                    job.prompt.as_deref(),
                    true,
                );
                let _ = job.result_tx.send(result);
            }
        })
        .map_err(|e| anyhow::anyhow!("failed to spawn whisper streaming inference thread: {e}"))?;

    Ok(job_tx)
}

/// Local whisper.cpp transcription backend.
pub struct LocalWhisperBackend {
    ctx: Option<Arc<whisper_rs::WhisperContext>>,
    #[allow(dead_code)]
    model_path: String,
}

impl LocalWhisperBackend {
    /// Create a new local whisper backend, eagerly loading the model.
    pub fn new(model_path: String) -> Self {
        let ctx = match Self::load_model(&model_path) {
            Ok(ctx) => {
                info!("loaded whisper model from {model_path}");
                Some(Arc::new(ctx))
            }
            Err(e) => {
                warn!("failed to load whisper model from {model_path}: {e}");
                None
            }
        };

        Self { ctx, model_path }
    }

    fn load_model(path: &str) -> anyhow::Result<whisper_rs::WhisperContext> {
        if !std::path::Path::new(path).exists() {
            anyhow::bail!("model file not found: {path}. Run 'whisrs setup' to download a model.");
        }

        let params = whisper_rs::WhisperContextParameters::default();

        whisper_rs::WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow::anyhow!("failed to initialize whisper context: {e}"))
    }
}

/// Convert i16 PCM samples to f32 in the range [-1.0, 1.0].
#[cfg(any(feature = "local-whisper", test))]
fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|&s| s as f32 / i16::MAX as f32)
        .collect()
}

fn inference_thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .min(8)
}

/// Run whisper inference on an audio window, reusing an existing state when
/// provided.
fn run_whisper_inference_on_state(
    state: &mut whisper_rs::WhisperState,
    audio: &[f32],
    language: &str,
    prompt: Option<&str>,
    streaming: bool,
) -> anyhow::Result<String> {
    use whisper_rs::{FullParams, SamplingStrategy};

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_n_threads(inference_thread_count());
    params.set_suppress_blank(true);
    // Block parenthetical sound descriptions like "(dog barking)" on silence.
    params.set_suppress_nst(true);

    if streaming {
        params.set_single_segment(true);
        // Each sliding window is a fresh decode; continuity comes from
        // `initial_prompt` (the committed transcript), not decoder state.
        params.set_no_context(true);
    } else {
        params.set_no_context(false);
    }

    if language != "auto" {
        params.set_language(Some(language));
    }

    if let Some(prompt) = prompt {
        if !prompt.is_empty() {
            params.set_initial_prompt(prompt);
        }
    }

    state
        .full(params, audio)
        .map_err(|e| anyhow::anyhow!("whisper inference failed: {e}"))?;

    Ok(collect_whisper_text(state))
}

/// True for whisper's common bracketed sound-effect hallucinations on silence.
fn is_parenthetical_sound_hallucination(text: &str) -> bool {
    let t = text.trim();
    let Some(inner) = t.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    let inner = inner.trim().to_lowercase();
    if inner.is_empty() {
        return true;
    }
    const MARKERS: &[&str] = &[
        "bark",
        "barking",
        "dog",
        "applause",
        "music",
        "silence",
        "pause",
        "laugh",
        "laughter",
        "sigh",
        "breath",
        "cough",
        "noise",
        "static",
        "inaudible",
        "unintelligible",
        "beep",
        "click",
        "crowd",
        "cheering",
        "whistle",
        "horn",
        "meow",
        "moo",
        "murmur",
        "mumbling",
        "footstep",
    ];
    MARKERS.iter().any(|marker| inner.contains(marker))
}

fn filter_whisper_segment(text: &str, no_speech_prob: f32) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if no_speech_prob > NO_SPEECH_SEGMENT_THRESHOLD {
        return None;
    }
    if is_parenthetical_sound_hallucination(t) {
        return None;
    }
    Some(t.to_string())
}

fn collect_whisper_text(state: &whisper_rs::WhisperState) -> String {
    let mut text = String::new();
    let Ok(n_segments) = state.full_n_segments() else {
        return String::new();
    };
    for i in 0..n_segments {
        let raw = state.full_get_segment_text_lossy(i).unwrap_or_default();
        // whisper-rs 0.14 does not expose per-segment no_speech probability.
        match filter_whisper_segment(&raw, 0.0) {
            Some(part) => text.push_str(&part),
            None if !raw.trim().is_empty() => {
                debug!(
                    "filtered whisper segment: {:?}",
                    raw
                );
            }
            None => {}
        }
    }
    text.trim().to_string()
}

/// Run whisper inference on an audio window.
///
/// - `prompt`: previous transcription to condition this window for consistency.
///   Whisper uses it to maintain context across overlapping windows.
fn run_whisper_inference(
    ctx: &whisper_rs::WhisperContext,
    audio: &[f32],
    language: &str,
    prompt: Option<&str>,
) -> anyhow::Result<String> {
    let mut state = ctx
        .create_state()
        .map_err(|e| anyhow::anyhow!("failed to create whisper state: {e}"))?;

    run_whisper_inference_on_state(&mut state, audio, language, prompt, false)
}

async fn infer_stream_window(
    infer_tx: &std::sync::mpsc::SyncSender<StreamInferJob>,
    samples: Vec<f32>,
    language: &str,
    prompt: Option<&str>,
) -> anyhow::Result<String> {
    let (result_tx, result_rx) = oneshot::channel();
    infer_tx
        .send(StreamInferJob {
            samples,
            language: language.to_string(),
            prompt: prompt.map(str::to_string),
            result_tx,
        })
        .map_err(|_| anyhow::anyhow!("whisper streaming inference thread exited"))?;

    result_rx
        .await
        .map_err(|_| anyhow::anyhow!("whisper streaming inference result dropped"))?
}

async fn process_stream_window(
    infer_tx: &std::sync::mpsc::SyncSender<StreamInferJob>,
    window: &[i16],
    language: &str,
    committed: &mut String,
    text_tx: &mpsc::Sender<String>,
    omit_initial_prompt: bool,
    review_before_paste: bool,
) {
    if audio_silence_gate::is_silent(window, INFERENCE_SILENCE_THRESHOLD) {
        return;
    }

    let samples_f32 = i16_to_f32(window);
    let prev_prompt = if review_before_paste || omit_initial_prompt || committed.is_empty() {
        None
    } else {
        let tail = prompt_tail(committed, PROMPT_TAIL_WORDS);
        if tail.is_empty() {
            None
        } else {
            Some(tail)
        }
    };

    match infer_stream_window(
        infer_tx,
        samples_f32,
        language,
        prev_prompt.as_deref(),
    )
    .await
    {
        Ok(full_text) => {
            let trimmed = full_text.trim();
            if trimmed.is_empty() {
                return;
            }
            if review_before_paste {
                debug!("review preview window: {:?}", trimmed);
                text_tx.send(trimmed.to_string()).await.ok();
                return;
            }
            let new_text = dedup_window_text(committed, trimmed);
            if new_text.trim().is_empty() {
                return;
            }
            if !committed.is_empty() && !committed.ends_with(' ') {
                committed.push(' ');
            }
            committed.push_str(new_text.trim());
            debug!("streaming window produced: {:?}", new_text);
            text_tx.send(new_text).await.ok();
        }
        Err(e) => warn!("whisper window inference failed: {e}"),
    }
}

#[async_trait]
impl TranscriptionBackend for LocalWhisperBackend {
    async fn transcribe(
        &self,
        audio: &[u8],
        config: &TranscriptionConfig,
    ) -> anyhow::Result<String> {
        let ctx = self.ctx.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "whisper model not loaded from {}. Run 'whisrs setup' to download a model.",
                self.model_path
            )
        })?;

        // Decode WAV to i16 samples, then convert to f32.
        let cursor = std::io::Cursor::new(audio);
        let reader = hound::WavReader::new(cursor)?;
        let samples_i16: Vec<i16> = reader.into_samples::<i16>().collect::<Result<_, _>>()?;
        let mut samples_f32 = vec![0.0f32; samples_i16.len()];
        whisper_rs::convert_integer_to_float_audio(&samples_i16, &mut samples_f32)
            .map_err(|e| anyhow::anyhow!("failed to convert audio: {e}"))?;

        let ctx = Arc::clone(ctx);
        let language = config.language.clone();
        let prompt = config.prompt.clone();

        tokio::task::spawn_blocking(move || {
            run_whisper_inference(&ctx, &samples_f32, &language, prompt.as_deref())
        })
        .await?
    }

    async fn transcribe_stream(
        &self,
        mut audio_rx: mpsc::Receiver<AudioChunk>,
        text_tx: mpsc::Sender<String>,
        config: &TranscriptionConfig,
    ) -> anyhow::Result<()> {
        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("whisper model not loaded from {}", self.model_path))?;

        let infer_tx = start_stream_infer_thread(Arc::clone(ctx))?;

        let mut buffer: Vec<i16> = Vec::new();
        let mut next_process_at = INITIAL_WINDOW_SAMPLES;
        let mut last_processed_end: usize = 0;
        let mut consecutive_silent_skips: u32 = 0;
        // Committed transcript fed back as initial_prompt tail for the next window.
        let mut committed = config.prompt.clone().unwrap_or_default();
        let mut speech_started = false;
        let review_before_paste = config.review_before_paste;

        while let Some(chunk) = audio_rx.recv().await {
            if !speech_started && !audio_silence_gate::is_silent(&chunk, audio_silence_gate::SILENCE_RMS_THRESHOLD) {
                speech_started = true;
            }
            buffer.extend_from_slice(&chunk);

            while buffer.len() >= next_process_at {
                if !speech_started {
                    debug!(
                        "leading silence — skipping inference until speech (at sample {})",
                        next_process_at
                    );
                    last_processed_end = next_process_at;
                    next_process_at += STEP_SAMPLES;
                    continue;
                }

                let window_size = if last_processed_end == 0 {
                    INITIAL_WINDOW_SAMPLES.min(buffer.len())
                } else {
                    WINDOW_SAMPLES.min(buffer.len())
                };

                let window_end = next_process_at;
                let window_start = window_end.saturating_sub(window_size);
                let window = buffer[window_start..window_end].to_vec();

                if audio_silence_gate::is_silent(&window, INFERENCE_SILENCE_THRESHOLD) {
                    debug!(
                        "skipping silent window at samples {}..{}",
                        window_start, window_end
                    );
                    consecutive_silent_skips += 1;
                } else {
                    let omit_prompt =
                        consecutive_silent_skips >= SILENT_SKIPS_BEFORE_OMIT_PROMPT;
                    consecutive_silent_skips = 0;
                    process_stream_window(
                        &infer_tx,
                        &window,
                        &config.language,
                        &mut committed,
                        &text_tx,
                        omit_prompt,
                        review_before_paste,
                    )
                    .await;
                }

                last_processed_end = window_end;
                next_process_at += STEP_SAMPLES;
            }
        }

        // Process remaining audio not covered by the last window.
        if buffer.len() > last_processed_end {
            let remaining_start = if buffer.len() - last_processed_end < SAMPLE_RATE {
                last_processed_end.saturating_sub(WINDOW_SAMPLES / 4)
            } else {
                last_processed_end
            };
            let remaining = buffer[remaining_start..].to_vec();

            process_stream_window(
                &infer_tx,
                &remaining,
                &config.language,
                &mut committed,
                &text_tx,
                false,
                review_before_paste,
            )
            .await;
        }

        Ok(())
    }

    fn supports_streaming(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_returns_error() {
        let backend = LocalWhisperBackend::new("/nonexistent/model.bin".to_string());
        let config = TranscriptionConfig {
            language: "en".to_string(),
            model: "base.en".to_string(),
            prompt: None,
            ..Default::default()
        };
        let err = backend.transcribe(&[1, 2, 3], &config).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not available")
                || msg.contains("not loaded")
                || msg.contains("not found"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn i16_to_f32_conversion() {
        let samples = vec![0i16, i16::MAX, i16::MIN];
        let f32_samples = i16_to_f32(&samples);
        assert_eq!(f32_samples[0], 0.0);
        assert!((f32_samples[1] - 1.0).abs() < 0.001);
        assert!((f32_samples[2] + 1.0).abs() < 0.001);
    }

    #[test]
    fn filters_dog_bark_hallucinations() {
        assert!(is_parenthetical_sound_hallucination("(dog barks)"));
        assert!(is_parenthetical_sound_hallucination("(dog barking)"));
        assert!(!is_parenthetical_sound_hallucination("Can you download"));
        assert!(filter_whisper_segment("(dog barks)", 0.1).is_none());
        assert_eq!(
            filter_whisper_segment("Can you download", 0.1).as_deref(),
            Some("Can you download")
        );
    }
}
