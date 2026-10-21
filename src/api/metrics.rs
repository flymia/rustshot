//! Prometheus text exposition at `/metrics`.
//!
//! Database backed gauges describe the whole deployment (identical on every
//! instance sharing a database), `rustshot_http_*` counters are per process.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Mutex;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use super::{ApiError, AppState};
use crate::db;

const UPLOAD_WINDOWS: &[(&str, i64)] = &[("1h", 3600), ("24h", 86_400), ("7d", 7 * 86_400)];
const KEY_WINDOW: (&str, i64) = ("24h", 86_400);

/// Request counts by method and status code.
#[derive(Default)]
pub struct HttpMetrics {
    requests: Mutex<BTreeMap<(&'static str, u16), u64>>,
}

impl HttpMetrics {
    fn record(&self, method: &Method, status: u16) {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        *requests.entry((method_label(method), status)).or_default() += 1;
    }

    fn snapshot(&self) -> BTreeMap<(&'static str, u16), u64> {
        self.requests.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Bounds label cardinality: arbitrary extension methods collapse into `other`.
fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

pub async fn track(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let res = next.run(req).await;
    state.http.record(&method, res.status().as_u16());
    res
}

pub async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(expected) = &state.config.metrics_token {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim());
        // Comparing digests keeps the comparison time independent of the secret.
        if given.is_none_or(|t| Sha256::digest(t) != Sha256::digest(expected)) {
            tracing::warn!("rejected /metrics request with missing or wrong token");
            return Err(ApiError::Unauthorized);
        }
    }

    let windows: Vec<i64> = UPLOAD_WINDOWS.iter().map(|(_, secs)| *secs).collect();
    let stats = db::stats(&state.ctx.db, &windows, KEY_WINDOW.1).await?;

    let mut out = Exposition::default();
    out.family("rustshot_build_info", "gauge", "Build information.");
    out.sample(
        "rustshot_build_info",
        &[("version", env!("CARGO_PKG_VERSION"))],
        1,
    );
    out.family(
        "rustshot_start_time_seconds",
        "gauge",
        "Process start time, unix seconds.",
    );
    out.sample("rustshot_start_time_seconds", &[], state.started_at);

    out.family("rustshot_users", "gauge", "Registered users by role.");
    out.sample("rustshot_users", &[("role", db::ROLE_ADMIN)], stats.admins);
    out.sample("rustshot_users", &[("role", db::ROLE_USER)], stats.users);
    out.family("rustshot_uploads", "gauge", "Stored screenshots.");
    out.sample("rustshot_uploads", &[], stats.uploads);
    out.family(
        "rustshot_storage_used_bytes",
        "gauge",
        "Total size of stored screenshots.",
    );
    out.sample("rustshot_storage_used_bytes", &[], stats.used_bytes);
    out.family(
        "rustshot_recent_uploads",
        "gauge",
        "Stored screenshots created within the time window.",
    );
    for ((label, _), count) in UPLOAD_WINDOWS.iter().zip(&stats.recent_uploads) {
        out.sample("rustshot_recent_uploads", &[("window", label)], *count);
    }
    if let Some(ts) = stats.last_upload_at {
        out.family(
            "rustshot_last_upload_timestamp_seconds",
            "gauge",
            "Creation time of the newest screenshot, unix seconds.",
        );
        out.sample("rustshot_last_upload_timestamp_seconds", &[], ts);
    }

    out.family("rustshot_api_keys", "gauge", "API keys by state.");
    out.sample("rustshot_api_keys", &[("state", "active")], stats.active_keys);
    out.sample("rustshot_api_keys", &[("state", "revoked")], stats.revoked_keys);
    out.family(
        "rustshot_api_keys_recently_used",
        "gauge",
        "Active API keys used within the time window.",
    );
    out.sample(
        "rustshot_api_keys_recently_used",
        &[("window", KEY_WINDOW.0)],
        stats.recently_used_keys,
    );

    out.family(
        "rustshot_http_requests_total",
        "counter",
        "HTTP requests handled by this process.",
    );
    for ((method, status), count) in state.http.snapshot() {
        let status = status.to_string();
        out.sample(
            "rustshot_http_requests_total",
            &[("method", method), ("status", &status)],
            count,
        );
    }

    Ok((
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        out.0,
    )
        .into_response())
}

#[derive(Default)]
struct Exposition(String);

impl Exposition {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}\n# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        self.0.push_str(name);
        if !labels.is_empty() {
            self.0.push('{');
            for (i, (key, val)) in labels.iter().enumerate() {
                if i > 0 {
                    self.0.push(',');
                }
                let val = val
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n");
                let _ = write!(self.0, "{key}=\"{val}\"");
            }
            self.0.push('}');
        }
        let _ = writeln!(self.0, " {value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposition_format() {
        let mut out = Exposition::default();
        out.family("m", "gauge", "Help text.");
        out.sample("m", &[], 1);
        out.sample("m", &[("a", "x\"y"), ("b", "z")], 2);
        assert_eq!(
            out.0,
            "# HELP m Help text.\n# TYPE m gauge\nm 1\nm{a=\"x\\\"y\",b=\"z\"} 2\n"
        );
    }
}
