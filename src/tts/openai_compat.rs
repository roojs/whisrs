//! OpenAI-compatible text-to-speech backend.
//!
//! Speaks the OpenAI `/v1/audio/speech` request shape (`{model, voice, input,
//! response_format}`) and returns WAV-encoded audio bytes. Groq, OpenAI, and
//! local servers (Kokoro, Supertonic, etc.) all expose this interface, so a
//! single backend covers them by varying the base URL and whether an API key
//! is sent.

use async_trait::async_trait;
use serde::Serialize;
use tracing::debug;

use crate::WhisrsError;

use super::TtsBackend;

/// OpenAI-compatible text-to-speech backend.
pub struct OpenAiCompatTts {
    client: reqwest::Client,
    /// Full speech endpoint URL (e.g. `https://api.openai.com/v1/audio/speech`).
    base_url: String,
    /// Optional API key. When `None`, no `Authorization` header is sent —
    /// local sidecars usually need no auth.
    api_key: Option<String>,
    model: String,
    voice: String,
    response_format: String,
}

impl OpenAiCompatTts {
    /// Create a new OpenAI-compatible TTS backend.
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        model: String,
        voice: String,
        response_format: String,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key: api_key.filter(|k| !k.is_empty()),
            model,
            voice,
            response_format,
        }
    }

    /// Build the JSON request body for the given input text.
    ///
    /// Exposed for unit testing the wire format without hitting the network.
    fn request_body<'a>(&'a self, text: &'a str) -> SpeechRequest<'a> {
        SpeechRequest {
            model: &self.model,
            voice: &self.voice,
            input: text,
            response_format: &self.response_format,
        }
    }
}

/// Request body for the OpenAI-compatible `/v1/audio/speech` endpoint.
#[derive(Debug, Serialize)]
struct SpeechRequest<'a> {
    model: &'a str,
    voice: &'a str,
    input: &'a str,
    response_format: &'a str,
}

#[async_trait]
impl TtsBackend for OpenAiCompatTts {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>, WhisrsError> {
        if text.trim().is_empty() {
            return Err(WhisrsError::Transcription(
                "cannot synthesize empty text".to_string(),
            ));
        }

        debug!(
            "sending {} chars to TTS at {} (model={}, voice={}, format={})",
            text.len(),
            self.base_url,
            self.model,
            self.voice,
            self.response_format
        );

        let mut request = self
            .client
            .post(&self.base_url)
            .json(&self.request_body(text));
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request
            .send()
            .await
            .map_err(|e| WhisrsError::Transcription(format!("TTS request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(WhisrsError::Transcription(format!(
                "TTS error ({}): {}",
                status.as_u16(),
                body
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| WhisrsError::Transcription(format!("TTS read body failed: {e}")))?;

        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_serializes_expected_shape() {
        let backend = OpenAiCompatTts::new(
            "https://api.openai.com/v1/audio/speech".to_string(),
            Some("test-key".to_string()),
            "tts-1".to_string(),
            "alloy".to_string(),
            "wav".to_string(),
        );
        let json = serde_json::to_value(backend.request_body("hello world")).unwrap();
        assert_eq!(json["model"], "tts-1");
        assert_eq!(json["voice"], "alloy");
        assert_eq!(json["input"], "hello world");
        assert_eq!(json["response_format"], "wav");
    }

    #[test]
    fn empty_api_key_is_normalized_to_none() {
        // Sidecars are configured without a key; an empty string must not
        // become a bogus "Authorization: Bearer " header.
        let backend = OpenAiCompatTts::new(
            "http://127.0.0.1:8880/v1/audio/speech".to_string(),
            Some(String::new()),
            "kokoro".to_string(),
            "af_heart".to_string(),
            "wav".to_string(),
        );
        assert!(backend.api_key.is_none());
    }

    #[tokio::test]
    async fn synthesize_rejects_empty_text() {
        let backend = OpenAiCompatTts::new(
            "https://api.groq.com/openai/v1/audio/speech".to_string(),
            Some("test-key".to_string()),
            "canopylabs/orpheus-v1-english".to_string(),
            "autumn".to_string(),
            "wav".to_string(),
        );
        let err = backend.synthesize("   ").await.unwrap_err();
        assert!(err.to_string().contains("empty text"));
    }
}
