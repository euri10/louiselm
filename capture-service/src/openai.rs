use std::{fs::File, time::Duration};

use reqwest::{StatusCode, blocking::multipart};
use serde::Deserialize;

use crate::{Transcriber, TranscriptRequest, TranscriptionError};

const TRANSCRIPTIONS_URL: &str = "https://api.openai.com/v1/audio/transcriptions";

/// Accuracy-first OpenAI transcription provider.
#[derive(Clone)]
pub struct OpenAiTranscriber {
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl OpenAiTranscriber {
    /// Configure OpenAI transcription with a separate API credential.
    ///
    /// # Errors
    ///
    /// Rejects empty credentials/models and HTTP-client construction failures.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, String> {
        let api_key = api_key.into();
        let model = model.into();
        if api_key.trim().is_empty() {
            return Err("OpenAI API key must not be empty".to_owned());
        }
        if model.trim().is_empty() {
            return Err("OpenAI transcription model must not be empty".to_owned());
        }
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "OpenAI HTTP client could not be created".to_owned())?;
        Ok(Self {
            api_key,
            model,
            client,
        })
    }
}

impl Transcriber for OpenAiTranscriber {
    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn transcribe(&self, request: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        let file = File::open(request.audio_path)
            .map_err(|_| TranscriptionError::permanent("original audio cannot be opened"))?;
        let length = file
            .metadata()
            .map_err(|_| TranscriptionError::permanent("original audio metadata cannot be read"))?
            .len();
        let filename = request
            .audio_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("audio");
        let audio = multipart::Part::reader_with_length(file, length)
            .file_name(filename.to_owned())
            .mime_str(request.mime_type)
            .map_err(|_| TranscriptionError::permanent("audio MIME type is invalid"))?;
        let form = multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", audio);
        let response = self
            .client
            .post(TRANSCRIPTIONS_URL)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .map_err(|_| TranscriptionError::transient("OpenAI transcription request failed"))?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        let response: TranscriptResponse = response.json().map_err(|_| {
            TranscriptionError::transient("OpenAI transcription response was malformed")
        })?;
        if response.text.trim().is_empty() {
            return Err(TranscriptionError::permanent(
                "OpenAI returned an empty transcript",
            ));
        }
        Ok(response.text)
    }
}

#[derive(Deserialize)]
struct TranscriptResponse {
    text: String,
}

fn status_error(status: StatusCode) -> TranscriptionError {
    if matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::CONFLICT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
    {
        return TranscriptionError::transient(format!(
            "OpenAI transcription is temporarily unavailable ({status})"
        ));
    }
    TranscriptionError::permanent(format!(
        "OpenAI transcription rejected the request ({status})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TranscriptionErrorKind;

    #[test]
    fn retry_classification_matches_operator_action() {
        assert_eq!(
            status_error(StatusCode::TOO_MANY_REQUESTS).kind(),
            TranscriptionErrorKind::Transient
        );
        assert_eq!(
            status_error(StatusCode::SERVICE_UNAVAILABLE).kind(),
            TranscriptionErrorKind::Transient
        );
        assert_eq!(
            status_error(StatusCode::UNAUTHORIZED).kind(),
            TranscriptionErrorKind::Permanent
        );
        assert_eq!(
            status_error(StatusCode::PAYLOAD_TOO_LARGE).kind(),
            TranscriptionErrorKind::Permanent
        );
    }
}
