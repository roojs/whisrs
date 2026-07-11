//! Lightweight RMS-based silence detection and auto-stop for audio capture.
//!
//! Used to detect when the user has stopped speaking, for auto-stop
//! and VAD-based chunk splitting.
//!
//! # no_std support
//!
//! This crate currently requires `std`. A `#![no_std]` variant is feasible
//! (only `f64::sqrt` and alloc-only `Vec` in tests) and may be gated
//! behind a feature flag in a future release.

/// Normalized RMS threshold below which audio is considered silence.
///
/// Shared between [`AutoStopDetector`] and [`audio_gate_reason`] so the gate
/// agrees with auto-stop on what counts as silence — empirically, recordings
/// whose entire duration sits under this floor carry no usable speech.
pub const SILENCE_RMS_THRESHOLD: f64 = 0.003;

/// Calculate the RMS (root mean square) energy of a slice of i16 samples.
///
/// Returns a value between 0.0 and 1.0 (normalized to the i16 range).
pub fn rms_energy(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_squares / samples.len() as f64).sqrt();

    // Normalize to 0.0–1.0 range.
    rms / i16::MAX as f64
}

/// Check if a chunk of audio is below the silence threshold.
///
/// `threshold` is a normalized RMS value (0.0–1.0). Typical speech
/// produces RMS around 0.02–0.15; silence is usually below 0.005.
pub fn is_silent(samples: &[i16], threshold: f64) -> bool {
    rms_energy(samples) < threshold
}

/// Reason a recorded buffer should be skipped instead of sent to a transcription
/// backend.
///
/// Returned by [`audio_gate_reason`]; carries a short human-readable label
/// that the daemon logs and surfaces in notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GateReason {
    /// Buffer contains zero samples.
    Empty,
    /// Recording shorter than the minimum allowed duration.
    TooShort,
    /// Recording's RMS energy is below the configured silence threshold for
    /// its full duration.
    Silent,
    /// Caller passed an invalid configuration (e.g. `sample_rate == 0`) so
    /// the gate cannot compute a meaningful verdict and conservatively
    /// rejects the buffer.
    Invalid,
}

impl GateReason {
    /// Short human-readable label suitable for logs and notifications.
    pub fn as_str(self) -> &'static str {
        match self {
            GateReason::Empty => "empty",
            GateReason::TooShort => "too short",
            GateReason::Silent => "silent",
            GateReason::Invalid => "invalid",
        }
    }
}

/// Decide whether an audio buffer is too empty/silent to bother sending to a
/// cloud transcription backend.
///
/// Returns `Some(reason)` when the buffer should be discarded outright, `None`
/// otherwise. Thresholds are intentionally permissive — the goal is to filter
/// out the obviously unusable cases (accidental hotkey taps, recordings that
/// captured only background hum) that would otherwise reach the API. Cloud
/// Whisper variants (whisper-1, gpt-4o-*-transcribe) hallucinate verbatim
/// chunks of the supplied `prompt` when the audio carries no speech, so this
/// gate is the first line of defence against prompt-echo output.
///
/// # Contract
///
/// `sample_rate` is expected to be the recording sample rate in Hz (e.g.
/// `16_000`) and must be greater than zero. If `sample_rate == 0` is passed
/// (a misconfiguration) the function does **not** panic: it returns
/// `Some(GateReason::Invalid)`, since there is no way to compute a
/// meaningful duration without a sample rate, and the safest default is to
/// treat the buffer as ungatable and skip it.
pub fn audio_gate_reason(
    samples: &[i16],
    sample_rate: u32,
    min_duration_ms: u64,
    threshold: f64,
) -> Option<GateReason> {
    if samples.is_empty() {
        return Some(GateReason::Empty);
    }
    if sample_rate == 0 {
        // Cannot compute duration without a sample rate; conservatively skip
        // the buffer rather than dividing by zero. This is a misconfiguration
        // distinct from a too-short recording, so report it as Invalid.
        return Some(GateReason::Invalid);
    }
    let duration_ms = (samples.len() as u64).saturating_mul(1000) / sample_rate as u64;
    if duration_ms < min_duration_ms {
        return Some(GateReason::TooShort);
    }
    if is_silent(samples, threshold) {
        return Some(GateReason::Silent);
    }
    None
}

/// Tracks consecutive silent frames and signals when silence has exceeded
/// a configured timeout, enabling auto-stop of recording.
pub struct AutoStopDetector {
    /// Silence threshold (normalized RMS, 0.0–1.0).
    threshold: f64,
    /// Number of consecutive silent samples accumulated.
    silent_samples: u64,
    /// Number of consecutive silent samples required to trigger auto-stop.
    timeout_samples: u64,
    /// Whether any speech has been detected yet (avoid auto-stop before
    /// the user starts speaking).
    speech_detected: bool,
}

impl AutoStopDetector {
    /// Create a new auto-stop detector.
    ///
    /// - `threshold`: RMS silence threshold (e.g. 0.005).
    /// - `timeout_ms`: Duration of continuous silence (in milliseconds) to trigger stop.
    /// - `sample_rate`: Audio sample rate in Hz (e.g. 16000).
    ///
    /// # Notes
    ///
    /// `sample_rate` and `timeout_ms` are both expected to be greater than
    /// zero. The constructor does **not** panic on `0` for either argument,
    /// but `timeout_samples` (the threshold [`feed`](Self::feed) compares
    /// against) is saturated to a minimum of `1` so that the detector cannot
    /// auto-stop on the very first speech sample (which would otherwise
    /// happen because `silent_samples >= 0` is trivially true). Treat
    /// `0`-valued arguments as a misconfiguration: the detector will still
    /// behave sanely, but the timeout it enforces will be effectively a
    /// single sample rather than the duration the caller intended.
    pub fn new(threshold: f64, timeout_ms: u64, sample_rate: u32) -> Self {
        // Saturate to >=1 so that a misconfigured `sample_rate == 0` or
        // `timeout_ms == 0` does not yield `timeout_samples == 0`, which
        // would cause `feed()` to auto-stop on the first detected speech
        // chunk (since `silent_samples >= 0` is trivially true once
        // `speech_detected` flips).
        let timeout_samples = ((timeout_ms.saturating_mul(sample_rate as u64)) / 1000).max(1);
        Self {
            threshold,
            silent_samples: 0,
            timeout_samples,
            speech_detected: false,
        }
    }

    /// Feed a chunk of audio samples and return `true` if auto-stop
    /// should be triggered.
    pub fn feed(&mut self, samples: &[i16]) -> bool {
        if is_silent(samples, self.threshold) {
            self.silent_samples += samples.len() as u64;
        } else {
            self.silent_samples = 0;
            self.speech_detected = true;
        }

        // Only trigger auto-stop after speech has been detected.
        self.speech_detected && self.silent_samples >= self.timeout_samples
    }

    /// Reset the detector state.
    pub fn reset(&mut self) {
        self.silent_samples = 0;
        self.speech_detected = false;
    }

    /// Whether any speech has been detected since creation or last reset.
    pub fn has_speech(&self) -> bool {
        self.speech_detected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_zero_rms() {
        let silence = vec![0i16; 1600];
        assert_eq!(rms_energy(&silence), 0.0);
        assert!(is_silent(&silence, 0.01));
    }

    #[test]
    fn empty_slice_is_zero_rms() {
        assert_eq!(rms_energy(&[]), 0.0);
    }

    #[test]
    fn loud_signal_has_high_rms() {
        let loud: Vec<i16> = vec![i16::MAX; 1600];
        let rms = rms_energy(&loud);
        assert!(rms > 0.9, "loud signal RMS should be near 1.0, got {rms}");
        assert!(!is_silent(&loud, 0.01));
    }

    #[test]
    fn quiet_signal_is_detected() {
        // Low-level samples (~1% of max).
        let quiet: Vec<i16> = (0..1600).map(|i| ((i % 100) as i16) - 50).collect();
        let rms = rms_energy(&quiet);
        assert!(rms < 0.01, "quiet signal RMS should be low, got {rms}");
        assert!(is_silent(&quiet, 0.01));
    }

    #[test]
    fn medium_signal() {
        // ~50% amplitude sine-ish wave.
        let medium: Vec<i16> = (0..1600)
            .map(|i| ((i as f64 * 0.1).sin() * 16000.0) as i16)
            .collect();
        let rms = rms_energy(&medium);
        assert!(rms > 0.1, "medium signal should have noticeable RMS");
        assert!(!is_silent(&medium, 0.01));
    }

    // --- AutoStopDetector tests ---

    #[test]
    fn auto_stop_not_triggered_without_speech() {
        // 16kHz, 2000ms timeout, threshold 0.01.
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);

        // Feed 3 seconds of silence — should NOT trigger because no speech detected.
        let silence = vec![0i16; 16_000]; // 1 second
        assert!(!detector.feed(&silence));
        assert!(!detector.feed(&silence));
        assert!(!detector.feed(&silence));
        assert!(!detector.has_speech());
    }

    #[test]
    fn auto_stop_triggered_after_speech_then_silence() {
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);

        // Feed some loud audio (speech).
        let loud: Vec<i16> = vec![10000; 16_000]; // 1 second of speech
        assert!(!detector.feed(&loud));
        assert!(detector.has_speech());

        // Feed 1 second of silence — not enough yet (need 2000ms).
        let silence = vec![0i16; 16_000];
        assert!(!detector.feed(&silence));

        // Feed another second — now 2 seconds of silence, should trigger.
        assert!(detector.feed(&silence));
    }

    #[test]
    fn auto_stop_resets_on_speech() {
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);

        let loud: Vec<i16> = vec![10000; 16_000];
        let silence = vec![0i16; 16_000];

        // Speech, then 1.5s silence.
        detector.feed(&loud);
        detector.feed(&silence);
        // Half second more silence.
        let half_sec = vec![0i16; 8_000];
        assert!(!detector.feed(&half_sec));

        // More speech — resets silence counter.
        assert!(!detector.feed(&loud));

        // 1.5s silence again — not enough.
        detector.feed(&silence);
        assert!(!detector.feed(&half_sec));

        // Another 0.5s — now 2s total since last speech.
        assert!(detector.feed(&half_sec));
    }

    #[test]
    fn auto_stop_reset() {
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);

        let loud: Vec<i16> = vec![10000; 16_000];
        detector.feed(&loud);
        assert!(detector.has_speech());

        detector.reset();
        assert!(!detector.has_speech());
    }

    // --- audio_gate_reason tests ---

    #[test]
    fn gate_rejects_empty() {
        assert_eq!(
            audio_gate_reason(&[], 16_000, 300, 0.005),
            Some(GateReason::Empty)
        );
    }

    #[test]
    fn gate_rejects_too_short() {
        // 100ms at 16kHz = 1600 samples; threshold 300ms.
        let samples: Vec<i16> = vec![10_000; 1_600];
        assert_eq!(
            audio_gate_reason(&samples, 16_000, 300, 0.005),
            Some(GateReason::TooShort)
        );
    }

    #[test]
    fn gate_rejects_silent_long_recording() {
        // 1s of pure silence — long enough but below threshold.
        let samples = vec![0i16; 16_000];
        assert_eq!(
            audio_gate_reason(&samples, 16_000, 300, 0.005),
            Some(GateReason::Silent)
        );
    }

    #[test]
    fn gate_accepts_speech() {
        // ~50% amplitude tone, 1s — clearly above threshold.
        let samples: Vec<i16> = (0..16_000)
            .map(|i| ((i as f64 * 0.1).sin() * 16_000.0) as i16)
            .collect();
        assert_eq!(audio_gate_reason(&samples, 16_000, 300, 0.005), None);
    }

    #[test]
    fn gate_handles_zero_sample_rate_without_panic() {
        // Non-empty buffer with sample_rate = 0 must not panic (would have
        // divided by zero pre-fix). Reported as Invalid — a misconfiguration,
        // distinct from a duration verdict like TooShort.
        let samples: Vec<i16> = vec![10_000; 1_600];
        assert_eq!(
            audio_gate_reason(&samples, 0, 300, 0.005),
            Some(GateReason::Invalid)
        );

        // Empty buffer with sample_rate = 0 still hits the Empty path first.
        assert_eq!(
            audio_gate_reason(&[], 0, 300, 0.005),
            Some(GateReason::Empty)
        );
    }

    #[test]
    fn auto_stop_handles_zero_sample_rate() {
        // sample_rate = 0 must not yield timeout_samples = 0 (which would
        // make feed() auto-stop on the first speech chunk because
        // `silent_samples >= 0` is trivially true). Saturated to >=1.
        let mut detector = AutoStopDetector::new(0.01, 2000, 0);

        // Speech alone should NOT trigger auto-stop — speech_detected flips
        // but silent_samples is reset to 0, which is below the saturated
        // timeout of 1.
        let loud: Vec<i16> = vec![10_000; 1_600];
        assert!(!detector.feed(&loud));
        assert!(detector.has_speech());

        // A single silent sample is enough to trip the saturated timeout
        // of 1 once speech has been detected — this is the expected
        // misconfiguration behavior, documented on `new`.
        let silence = vec![0i16; 1];
        assert!(detector.feed(&silence));
    }

    #[test]
    fn auto_stop_handles_zero_timeout() {
        // timeout_ms = 0 must not yield timeout_samples = 0 either; same
        // saturation contract as zero sample_rate.
        let mut detector = AutoStopDetector::new(0.01, 0, 16_000);

        let loud: Vec<i16> = vec![10_000; 1_600];
        assert!(!detector.feed(&loud));
        assert!(detector.has_speech());

        // Saturated timeout = 1 sample; one silent sample trips it.
        let silence = vec![0i16; 1];
        assert!(detector.feed(&silence));
    }

    #[test]
    fn auto_stop_exact_threshold() {
        // Timeout of exactly 1600 samples (100ms at 16kHz).
        let mut detector = AutoStopDetector::new(0.01, 100, 16_000);

        let loud: Vec<i16> = vec![10000; 1600];
        detector.feed(&loud);

        // Feed exactly 1600 silent samples.
        let silence = vec![0i16; 1600];
        assert!(detector.feed(&silence));
    }
}
