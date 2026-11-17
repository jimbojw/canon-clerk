// Copyright 2026 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Persistent background Tokio reactor and hardened pooled HTTP transport helpers.

use super::gemini_wire::JSON_MIME_TYPE;
use crate::provider::ProviderError;
use serde::Serialize;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

/// Default TCP + TLS connection establishment timeout for pooled provider HTTP clients.
pub(crate) const DEFAULT_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default idle retention window for pooled keep-alive sockets (`< 60s` NAT/GFE eviction).
pub(crate) const DEFAULT_HTTP_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Default idle delay before OS-level TCP keep-alive probes begin on pooled sockets.
pub(crate) const DEFAULT_TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(10);

/// Default interval between OS-level TCP keep-alive probes (`10s + 3 * 5s = 25s < 60s`).
pub(crate) const DEFAULT_TCP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Default unacknowledged TCP keep-alive probe count before declaring a socket dead.
pub(crate) const DEFAULT_TCP_KEEPALIVE_RETRIES: u32 = 3;

static HTTP_REACTOR: OnceLock<Result<tokio::runtime::Handle, String>> = OnceLock::new();

/// Raw status, optional `Retry-After` header, decoded UTF-8 body, and wire latency from an outbound JSON `POST`.
pub(crate) struct RawHttpOutcome {
    pub(crate) status_code: u16,
    pub(crate) retry_after: Option<String>,
    pub(crate) body: String,
    pub(crate) latency_ms: u64,
}

/// Outbound authentication header scheme for a provider HTTP request.
#[derive(Clone, Copy)]
pub(crate) enum HttpAuth<'a> {
    /// Credential-free localhost HTTP transport (`local:*`).
    None,
    /// Google Generative Language API `x-goog-api-key` header.
    GoogApiKey(&'a str),
    /// Standard `Authorization: Bearer <token>` header.
    Bearer(&'a str),
}

/// Owned authentication header scheme carried across async reactor boundaries.
#[derive(Clone)]
pub(crate) enum OwnedHttpAuth {
    /// Credential-free localhost HTTP transport (`local:*`).
    None,
    /// Google Generative Language API `x-goog-api-key` header.
    GoogApiKey(String),
    /// Standard `Authorization: Bearer <token>` header.
    Bearer(String),
}

impl HttpAuth<'_> {
    pub(crate) fn into_owned(self) -> OwnedHttpAuth {
        match self {
            Self::None => OwnedHttpAuth::None,
            Self::GoogApiKey(key) => OwnedHttpAuth::GoogApiKey(key.to_string()),
            Self::Bearer(token) => OwnedHttpAuth::Bearer(token.to_string()),
        }
    }
}

/// Outbound JSON `POST` envelope parameters executed on the persistent background reactor.
pub(crate) struct JsonPostRequest<'a> {
    pub(crate) url: &'a str,
    pub(crate) model_specifier: &'a str,
    pub(crate) auth: HttpAuth<'a>,
    pub(crate) timeout_ms: u64,
}

/// Owned JSON `POST` envelope parameters used by retryable async tasks on the HTTP reactor.
pub(crate) struct OwnedJsonPostRequest {
    pub(crate) url: String,
    pub(crate) model_specifier: String,
    pub(crate) auth: OwnedHttpAuth,
    pub(crate) timeout_ms: u64,
    pub(crate) call_label: Option<String>,
}

/// Builds a connection-pooled [`reqwest::Client`] hardened against stale idle sockets,
/// silent NAT blackholes, and blackholed TCP/TLS handshakes.
pub(crate) fn build_pooled_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(DEFAULT_HTTP_CONNECT_TIMEOUT)
        .pool_idle_timeout(DEFAULT_HTTP_POOL_IDLE_TIMEOUT)
        .tcp_keepalive(DEFAULT_TCP_KEEPALIVE_IDLE)
        .tcp_keepalive_interval(DEFAULT_TCP_KEEPALIVE_INTERVAL)
        .tcp_keepalive_retries(DEFAULT_TCP_KEEPALIVE_RETRIES)
        .build()
        .unwrap_or_default()
}

fn spawn_background_http_reactor() -> Result<tokio::runtime::Handle, String> {
    let (init_tx, init_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("canon-clerk-http".to_string())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    let handle = runtime.handle().clone();
                    if init_tx.send(Ok(handle)).is_ok() {
                        runtime.block_on(std::future::pending::<()>());
                    }
                }
                Err(err) => {
                    let _ = init_tx.send(Err(err.to_string()));
                }
            }
        })
        .map_err(|err| err.to_string())?;

    init_rx
        .recv()
        .map_err(|err| format!("background HTTP reactor exited during startup ({err})"))?
}

pub(crate) fn http_reactor_handle(
    model_specifier: &str,
) -> Result<&'static tokio::runtime::Handle, (ProviderError, u64)> {
    match HTTP_REACTOR.get_or_init(spawn_background_http_reactor) {
        Ok(handle) => Ok(handle),
        Err(err) => Err((
            ProviderError::Transport {
                model_specifier: model_specifier.to_string(),
                message: format!("failed to initialize persistent async HTTP runtime ({err})"),
            },
            0,
        )),
    }
}

fn format_reqwest_error_chain(err: &reqwest::Error) -> String {
    use std::error::Error as _;

    let mut msg = err.to_string();
    let mut current = err.source();
    while let Some(cause) = current {
        let cause_text = cause.to_string();
        if !cause_text.is_empty() && !msg.contains(&cause_text) {
            msg.push_str(": ");
            msg.push_str(&cause_text);
        }
        current = cause.source();
    }
    msg
}

pub(crate) fn classify_reqwest_error(
    err: &reqwest::Error,
    model_specifier: &str,
    timeout_ms: u64,
) -> ProviderError {
    if err.is_connect() {
        ProviderError::Transport {
            model_specifier: model_specifier.to_string(),
            message: format_reqwest_error_chain(err),
        }
    } else if err.is_timeout() {
        ProviderError::Timeout {
            model_specifier: model_specifier.to_string(),
            timeout_ms,
        }
    } else {
        ProviderError::Transport {
            model_specifier: model_specifier.to_string(),
            message: format_reqwest_error_chain(err),
        }
    }
}

pub(crate) fn serialize_json_payload<T: Serialize>(
    payload: &T,
    model_specifier: &str,
) -> Result<Vec<u8>, (ProviderError, u64)> {
    let start = Instant::now();
    serde_json::to_vec(payload).map_err(|err| {
        let latency_ms = start.elapsed().as_millis() as u64;
        (
            ProviderError::Transport {
                model_specifier: model_specifier.to_string(),
                message: format!("failed to serialize JSON request payload ({err})"),
            },
            latency_ms,
        )
    })
}

/// Dispatches a single wire attempt with a fresh per-attempt socket timeout (`req.timeout_ms`).
pub(crate) async fn execute_wire_attempt(
    client: &reqwest::Client,
    req: &OwnedJsonPostRequest,
    body_bytes: Vec<u8>,
) -> Result<RawHttpOutcome, (ProviderError, u64)> {
    let wire_start = Instant::now();
    let builder = client
        .post(&req.url)
        .timeout(Duration::from_millis(req.timeout_ms))
        .header("Content-Type", JSON_MIME_TYPE);
    let builder = match &req.auth {
        OwnedHttpAuth::None => builder,
        OwnedHttpAuth::GoogApiKey(key) => builder.header("x-goog-api-key", key),
        OwnedHttpAuth::Bearer(token) => builder.bearer_auth(token),
    };
    let http_request = builder.body(body_bytes).build().map_err(|err| {
        let latency_ms = wire_start.elapsed().as_millis() as u64;
        (
            classify_reqwest_error(&err, &req.model_specifier, req.timeout_ms),
            latency_ms,
        )
    })?;

    let response = client.execute(http_request).await.map_err(|err| {
        let latency_ms = wire_start.elapsed().as_millis() as u64;
        (
            classify_reqwest_error(&err, &req.model_specifier, req.timeout_ms),
            latency_ms,
        )
    })?;

    let status_code = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let body = response.text().await.map_err(|err| {
        let latency_ms = wire_start.elapsed().as_millis() as u64;
        (
            classify_reqwest_error(&err, &req.model_specifier, req.timeout_ms),
            latency_ms,
        )
    })?;
    let latency_ms = wire_start.elapsed().as_millis() as u64;

    Ok(RawHttpOutcome {
        status_code,
        retry_after,
        body,
        latency_ms,
    })
}

/// Serializes `payload` as JSON on the caller thread and drives a single-shot HTTP `POST` + full
/// body read on the persistent background Tokio reactor (`0` retries, bypasses cooldown gates).
pub(crate) fn execute_json_post<T: Serialize>(
    client: &reqwest::Client,
    req: &JsonPostRequest<'_>,
    payload: &T,
) -> Result<RawHttpOutcome, (ProviderError, u64)> {
    let start = Instant::now();
    let body_bytes = serialize_json_payload(payload, req.model_specifier)?;
    let handle = http_reactor_handle(req.model_specifier)?;
    let client_clone = client.clone();
    let owned_req = OwnedJsonPostRequest {
        url: req.url.to_string(),
        model_specifier: req.model_specifier.to_string(),
        auth: req.auth.into_owned(),
        timeout_ms: req.timeout_ms,
        call_label: None,
    };
    let (done_tx, done_rx) = mpsc::sync_channel(1);

    handle.spawn(async move {
        let outcome = execute_wire_attempt(&client_clone, &owned_req, body_bytes).await;
        let _ = done_tx.send(outcome);
    });

    done_rx.recv().map_err(|_| {
        let latency_ms = start.elapsed().as_millis() as u64;
        (
            ProviderError::Transport {
                model_specifier: req.model_specifier.to_string(),
                message: "background HTTP request task terminated unexpectedly".to_string(),
            },
            latency_ms,
        )
    })?
}
