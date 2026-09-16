//! Bounded HTTP v1 FCM sender with fixed payload and in-memory OAuth tokens.

mod auth;
#[cfg(test)]
mod tests;

use std::{
    io::Read,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

use reqwest::{
    StatusCode,
    blocking::{Client, Request},
    header,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::NotificationFailure;
use auth::{Credentials, TOKEN_URL};

const RESPONSE_LIMIT: u64 = 16 * 1024;
const COLLAPSE_KEY: &str = "louiselm-attention";

/// Owned by the notification worker and used only on a blocking thread.
pub(crate) struct FcmSender {
    credentials: Credentials,
    client: Client,
    access: Option<AccessToken>,
}

struct AccessToken {
    value: String,
    expires_at_ms: u64,
}

#[derive(Deserialize)]
struct Accepted {
    name: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
}

struct Reply {
    status: StatusCode,
    retry_after: Option<String>,
    body: Vec<u8>,
}

impl FcmSender {
    pub fn load(path: &Path) -> Result<Self, NotificationFailure> {
        Self::new(Credentials::load(path)?)
    }

    fn new(credentials: Credentials) -> Result<Self, NotificationFailure> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .build()
            .map_err(|_| NotificationFailure::Configuration)?;
        Ok(Self {
            credentials,
            client,
            access: None,
        })
    }

    pub fn send(
        &mut self,
        token: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), NotificationFailure> {
        let client = self.client.clone();
        self.send_with(token, generation, now_ms, |request| {
            execute(&client, request)
        })
    }

    // The fake transport receives the exact production requests, including OAuth.
    fn send_with(
        &mut self,
        token: &str,
        generation: u64,
        now_ms: u64,
        mut transport: impl FnMut(Request) -> Result<Reply, NotificationFailure>,
    ) -> Result<(), NotificationFailure> {
        self.authorize(now_ms, &mut transport)?;
        let access = self
            .access
            .as_ref()
            .ok_or(NotificationFailure::Authentication)?;
        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.credentials.project
        );
        let request = self
            .client
            .post(url)
            .bearer_auth(&access.value)
            .json(&payload(token, generation))
            .build()
            .map_err(|_| NotificationFailure::Configuration)?;
        let reply = transport(request)?;
        if reply.status == StatusCode::UNAUTHORIZED {
            self.access = None;
        }
        check_reply(&reply, now_ms, false)?;
        let accepted: Accepted =
            serde_json::from_slice(&reply.body).map_err(|_| NotificationFailure::Configuration)?;
        let prefix = format!("projects/{}/messages/", self.credentials.project);
        if accepted
            .name
            .strip_prefix(&prefix)
            .is_none_or(str::is_empty)
        {
            return Err(NotificationFailure::Configuration);
        }
        Ok(())
    }

    fn authorize(
        &mut self,
        now_ms: u64,
        transport: &mut impl FnMut(Request) -> Result<Reply, NotificationFailure>,
    ) -> Result<(), NotificationFailure> {
        if self
            .access
            .as_ref()
            .is_some_and(|access| access.expires_at_ms > now_ms.saturating_add(60_000))
        {
            return Ok(());
        }
        self.access = None;
        let assertion = self.credentials.assertion(now_ms)?;
        let request = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .build()
            .map_err(|_| NotificationFailure::Configuration)?;
        let reply = transport(request)?;
        check_reply(&reply, now_ms, true)?;
        let token: TokenResponse =
            serde_json::from_slice(&reply.body).map_err(|_| NotificationFailure::Configuration)?;
        if !token.token_type.eq_ignore_ascii_case("bearer")
            || !(61..=3_600).contains(&token.expires_in)
            || token.access_token.is_empty()
            || token.access_token.len() > 8 * 1024
            || !token.access_token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(NotificationFailure::Configuration);
        }
        self.access = Some(AccessToken {
            value: token.access_token,
            expires_at_ms: now_ms.saturating_add(token.expires_in.saturating_mul(1_000)),
        });
        Ok(())
    }
}

fn payload(token: &str, generation: u64) -> Value {
    // Explicit allowlist: neither Attention items nor caller-authored display fields
    // can enter this boundary. The token is required FCM routing metadata.
    json!({"message": {
        "token": token,
        "data": {"generation": generation.to_string()},
        "android": {"collapse_key": COLLAPSE_KEY, "priority": "high"}
    }})
}

fn execute(client: &Client, request: Request) -> Result<Reply, NotificationFailure> {
    let response = client
        .execute(request)
        .map_err(|_| NotificationFailure::Transient { retry_after_ms: 0 })?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    response
        .take(RESPONSE_LIMIT + 1)
        .read_to_end(&mut body)
        .map_err(|_| NotificationFailure::Transient { retry_after_ms: 0 })?;
    if body.len() as u64 > RESPONSE_LIMIT {
        return Err(NotificationFailure::Configuration);
    }
    Ok(Reply {
        status,
        retry_after,
        body,
    })
}

fn check_reply(reply: &Reply, now_ms: u64, oauth: bool) -> Result<(), NotificationFailure> {
    if reply.status == StatusCode::OK {
        return Ok(());
    }
    if reply.status == StatusCode::TOO_MANY_REQUESTS
        || reply.status == StatusCode::REQUEST_TIMEOUT
        || reply.status.is_server_error()
    {
        return Err(NotificationFailure::Transient {
            retry_after_ms: retry_delay(reply.retry_after.as_deref(), now_ms),
        });
    }
    if !oauth {
        let body: Value =
            serde_json::from_slice(&reply.body).map_err(|_| NotificationFailure::Configuration)?;
        let invalid_token = body
            .pointer("/error/details")
            .and_then(Value::as_array)
            .is_some_and(|details| {
                details.iter().any(|detail| {
                    detail.get("@type").and_then(Value::as_str)
                        == Some("type.googleapis.com/google.firebase.fcm.v1.FcmError")
                        && matches!(
                            detail.get("errorCode").and_then(Value::as_str),
                            Some("UNREGISTERED" | "INVALID_ARGUMENT")
                        )
                })
            });
        if matches!(
            reply.status,
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
        ) && invalid_token
        {
            return Err(NotificationFailure::InvalidToken);
        }
    }
    if matches!(
        reply.status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) || (oauth && reply.status == StatusCode::BAD_REQUEST)
    {
        return Err(NotificationFailure::Authentication);
    }
    Err(NotificationFailure::Configuration)
}

fn retry_delay(value: Option<&str>, now_ms: u64) -> u64 {
    let Some(value) = value else { return 0 };
    if let Ok(seconds) = value.parse::<u64>() {
        return seconds.saturating_mul(1_000);
    }
    httpdate::parse_http_date(value)
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .map_or(0, |retry_at| retry_at.saturating_sub(now_ms))
}
