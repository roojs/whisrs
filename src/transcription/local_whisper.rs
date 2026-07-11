//! Local whisper.cpp transcription backend via `whisper-rs`.
//!
//! Uses whisper-rs with a sliding window approach for pseudo-streaming.
//!
//! Deduplication strategy: each window is transcribed with `set_initial_prompt()`
//! set to the previous output for consistency. Text-based n-gram overlap removal
//! (from `dedup.rs`) strips the repeated prefix. Timestamp filtering is not used
//! because whisper often produces a single segment with `start_timestamp=0`,
//! making timestamp-based approaches unreliable.

use std::sync::Arc;

use asr_dedup::TextDedup;
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::{TranscriptionBackend, TranscriptionConfig};
use crate::audio::AudioChunk;

/// Sliding window parameters for pseudo-streaming.
///
/// Aligned with whisper.cpp `whisper-stream` (500 ms step, ~3 s context) for
/// lower latency; a 1 s first window improves time-to-first-token.
const WINDOW_SECS: usize = 3;
const STEP_MS: usize = 500;
const SAMPLE_RATE: usize = 16_000;
const WINDOW_SAMPLES: usize = WINDOW_SECS * SAMPLE_RATE;
const STEP_SAMPLES: usize = STEP_MS * SAMPLE_RATE / 1000;
/// Shorter initial window for faster first result.
const INITIAL_WINDOW_SECS: usize = 1;
const INITIAL_WINDOW_SAMPLES: usize = INITIAL_WINDOW_SECS * SAMPLE_RATE;
/// Whisper mel frames are 10 ms (16 kHz / hop 160).
const WHISPER_FRAME_MS: usize = 10;

/// Silence threshold — must match or be below the daemon's auto-stop threshold
/// (0.003) so we never skip windows that auto-stop considers speech.
const SILENCE_THRESHOLD: f64 = 0.003;

struct StreamInferJob {
    samples: Vec<f32>,
    language: String,
    prompt: Option<String>,
    result_tx: oneshot::Sender<anyhow::Result<String>>,
}

/// Long-lived inference worker started when the model loads. Avoids spawning a
/// thread and allocating GPU buffers on every recording toggle.
struct StreamInferWorker {
    job_tx: std::sync::mpsc::SyncSender<StreamInferJob>,
}

impl StreamInferWorker {
    fn start(ctx: Arc<whisper_rs::WhisperContext>) -> anyhow::Result<Self> {
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

                info!("whisper streaming inference worker ready");

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

        Ok(Self { job_tx })
    }
}

/// Local whisper.cpp transcription backend.
pub struct LocalWhisperBackend {
    ctx: Option<Arc<whisper_rs::WhisperContext>>,
    stream_infer: Option<StreamInferWorker>,
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

        let stream_infer = ctx.as_ref().and_then(|ctx| match StreamInferWorker::start(Arc::clone(ctx)) {
            Ok(worker) => Some(worker),
            Err(e) => {
                warn!("failed to start whisper streaming worker: {e}");
                None
            }
        });

        Self {
            ctx,
            stream_infer,
            model_path,
        }
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

fn streaming_audio_ctx(sample_count: usize) -> i32 {
    let window_ms = sample_count.saturating_mul(1000) / SAMPLE_RATE;
    let frames = window_ms.div_ceil(WHISPER_FRAME_MS);
    frames.max(1) as i32
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

    if language != "auto" {
        params.set_language(Some(language));
    }

    if let Some(prompt) = prompt {
        if !prompt.is_empty() {
            params.set_initial_prompt(prompt);
        }
    }

    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_n_threads(inference_thread_count());

    if streaming {
        // Match whisper-stream: faster decode, prompt carries cross-window context.
        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_audio_ctx(streaming_audio_ctx(audio.len()));
    } else {
        params.set_no_context(false);
    }

    state
        .full(params, audio)
        .map_err(|e| anyhow::anyhow!("whisper inference failed: {e}"))?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&format!("{}", segment));
    }

    Ok(text.trim().to_string())
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
    prompt: &mut String,
    dedup: &mut TextDedup,
    text_tx: &mpsc::Sender<String>,
) {
    if audio_silence_gate::is_silent(window, SILENCE_THRESHOLD) {
        return;
    }

    let samples_f32 = i16_to_f32(window);
    let prev_prompt = if prompt.is_empty() {
        None
    } else {
        Some(prompt.as_str())
    };

    match infer_stream_window(infer_tx, samples_f32, language, prev_prompt).await {
        Ok(full_text) => {
            if !full_text.is_empty() {
                prompt.clone_from(&full_text);
            }
            let new_text = dedup.filter_text(&full_text);
            if !new_text.trim().is_empty() {
                debug!("streaming window produced: {:?}", new_text);
                text_tx.send(new_text).await.ok();
            }
        }
        Err(e) => warn!("whisper window inference failed: {e}"),
    }
}

/// If inference fell behind real-time audio, skip stale windows and jump to
/// the latest step boundary so we type recent speech instead of a backlog.
fn coalesce_stale_windows(buffer_len: usize, next_process_at: usize) -> usize {
    let lag = buffer_len.saturating_sub(next_process_at);
    if lag <= STEP_SAMPLES {
        return next_process_at;
    }

    let steps_behind = lag / STEP_SAMPLES;
    let skip = steps_behind.saturating_sub(1) * STEP_SAMPLES;
    if skip > 0 {
        debug!("coalescing whisper windows: skipping {skip} samples ({steps_behind} steps behind)");
    }
    next_process_at + skip
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
        let infer_tx = self
            .stream_infer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("whisper model not loaded from {}", self.model_path))?
            .job_tx
            .clone();

        let mut buffer: Vec<i16> = Vec::new();
        let mut dedup = TextDedup::new();
        let mut next_process_at = INITIAL_WINDOW_SAMPLES;
        let mut last_processed_end: usize = 0;
        // Previous full transcription fed as prompt to the next window.
        // Seed with vocabulary prompt if available.
        let mut prompt = config.prompt.clone().unwrap_or_default();

        while let Some(chunk) = audio_rx.recv().await {
            buffer.extend_from_slice(&chunk);

            while buffer.len() >= next_process_at {
                next_process_at = coalesce_stale_windows(buffer.len(), next_process_at);
                if buffer.len() < next_process_at {
                    break;
                }

                let window_size = if last_processed_end == 0 {
                    INITIAL_WINDOW_SAMPLES.min(buffer.len())
                } else {
                    WINDOW_SAMPLES.min(buffer.len())
                };

                let window_end = next_process_at;
                let window_start = window_end.saturating_sub(window_size);
                let window = buffer[window_start..window_end].to_vec();

                if audio_silence_gate::is_silent(&window, SILENCE_THRESHOLD) {
                    debug!(
                        "skipping silent window at samples {}..{}",
                        window_start, window_end
                    );
                } else {
                    process_stream_window(
                        &infer_tx,
                        &window,
                        &config.language,
                        &mut prompt,
                        &mut dedup,
                        &text_tx,
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
                &mut prompt,
                &mut dedup,
                &text_tx,
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
    fn coalesce_skips_stale_steps() {
        assert_eq!(coalesce_stale_windows(16_000, 8_000), 8_000);
        // 3 steps behind → skip 2 steps.
        assert_eq!(coalesce_stale_windows(24_000, 8_000), 16_000);
    }
}
