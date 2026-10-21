use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use super::AppState;
use crate::db;

/// A check that takes longer than this counts as failed, so a hanging
/// database or NFS mount turns the instance unhealthy instead of stalling probes.
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
pub struct Health {
    status: &'static str,
    version: &'static str,
    uptime_seconds: u64,
    checks: Checks,
}

#[derive(Serialize)]
struct Checks {
    database: &'static str,
    storage: &'static str,
}

/// `200` when database and storage are usable, `503` otherwise. Failure
/// details are logged, not returned, since the endpoint is public.
pub async fn healthz(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    let ctx = &state.ctx;
    let (database, storage) = tokio::join!(
        check("database", db::ping(&ctx.db)),
        check("storage", ctx.storage.check_writable()),
    );
    let healthy = database && storage;
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let label = |ok: bool| if ok { "ok" } else { "error" };
    let body = Health {
        status: label(healthy),
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: state.started.elapsed().as_secs(),
        checks: Checks {
            database: label(database),
            storage: label(storage),
        },
    };
    (status, Json(body))
}

async fn check<E: std::fmt::Display>(name: &str, fut: impl Future<Output = Result<(), E>>) -> bool {
    match tokio::time::timeout(CHECK_TIMEOUT, fut).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::error!(check = name, error = %e, "health check failed");
            false
        }
        Err(_) => {
            tracing::error!(check = name, "health check timed out");
            false
        }
    }
}
