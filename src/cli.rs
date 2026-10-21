//! Command line interface: server, user/key administration and import.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use crate::api::{self, ApiConfig};
use crate::db::{self, NewUser, ROLE_ADMIN, ROLE_USER, User, UserUpdate};
use crate::service::{Ctx, validate_username};
use crate::size::{ByteSize, Count, Limit};
use crate::storage::Storage;

#[derive(Parser)]
#[command(name = "rustshot", version, about = "Self-hosted screenshot upload server")]
pub struct Cli {
    #[command(flatten)]
    global: GlobalOpts,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct GlobalOpts {
    /// Database URL, e.g. `sqlite://rustshot.db?mode=rwc` or `postgres://user:pass@host/db`
    #[arg(
        long,
        env = "RUSTSHOT_DATABASE_URL",
        default_value = "sqlite://rustshot.db?mode=rwc",
        global = true
    )]
    database_url: String,

    /// Maximum number of pooled database connections
    #[arg(
        long,
        env = "RUSTSHOT_DB_MAX_CONNECTIONS",
        default_value_t = 10,
        global = true
    )]
    db_max_connections: u32,

    /// Directory screenshots are stored in
    #[arg(long, env = "RUSTSHOT_STORAGE_DIR", default_value = "./data", global = true)]
    storage_dir: PathBuf,

    /// Public base URL used to build share links
    #[arg(
        long,
        env = "RUSTSHOT_PUBLIC_URL",
        default_value = "http://localhost:8080",
        global = true
    )]
    public_url: String,

    /// Do not apply pending database migrations on startup
    #[arg(long, env = "RUSTSHOT_SKIP_MIGRATIONS", global = true)]
    skip_migrations: bool,

    /// Log verbosity; `info` logs user actions such as uploads. Takes precedence over `RUST_LOG`
    /// [default: info]
    #[arg(long, env = "RUSTSHOT_LOG_LEVEL", value_enum, global = true)]
    log_level: Option<LogLevel>,
}

#[derive(Clone, Copy, ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

const DEFAULT_LOG_FILTER: &str = "info,sqlx=warn";

impl Cli {
    /// `--log-level` wins, then `RUST_LOG` (for fine-grained per-module filters), then `info`.
    pub fn log_filter(&self) -> EnvFilter {
        match self.global.log_level {
            Some(level) => EnvFilter::new(format!("{},sqlx=warn", level.as_str())),
            None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER)),
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server
    Serve(ServeOpts),
    /// Apply database migrations and exit
    Migrate,
    /// Manage users
    #[command(subcommand)]
    User(UserCmd),
    /// Manage API keys
    #[command(subcommand)]
    Key(KeyCmd),
    /// Import screenshots from the filesystem
    Import(crate::import::ImportOpts),
}

#[derive(Args)]
struct ServeOpts {
    /// Address to listen on
    #[arg(long, env = "RUSTSHOT_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    /// Maximum size of a single upload, e.g. `20MiB`
    #[arg(long, env = "RUSTSHOT_MAX_UPLOAD_SIZE", default_value = "20MiB")]
    max_upload_size: ByteSize,

    /// Comma separated origins allowed to use the API from a browser
    #[arg(long, env = "RUSTSHOT_CORS_ORIGINS", value_delimiter = ',')]
    cors_origins: Vec<String>,

    /// Bearer token Prometheus must send to scrape `/metrics`. Without it the endpoint is public
    #[arg(long, env = "RUSTSHOT_METRICS_TOKEN", hide_env_values = true)]
    metrics_token: Option<String>,
}

#[derive(Subcommand)]
enum UserCmd {
    /// Create a user and print an initial API key
    Create {
        username: String,
        /// Grant admin rights (can manage users through the API)
        #[arg(long)]
        admin: bool,
        /// Storage quota, e.g. `5GiB` or `unlimited`
        #[arg(long, default_value = "unlimited")]
        quota_bytes: Limit<ByteSize>,
        /// Maximum number of uploads or `unlimited`
        #[arg(long, default_value = "unlimited")]
        quota_uploads: Limit<Count>,
        /// Do not create an initial API key
        #[arg(long)]
        no_key: bool,
    },
    /// List all users with their usage
    List,
    /// Change role or quotas of a user
    Update {
        username: String,
        #[arg(long, value_parser = [ROLE_ADMIN, ROLE_USER])]
        role: Option<String>,
        /// Storage quota, e.g. `5GiB` or `unlimited`
        #[arg(long)]
        quota_bytes: Option<Limit<ByteSize>>,
        /// Maximum number of uploads or `unlimited`
        #[arg(long)]
        quota_uploads: Option<Limit<Count>>,
    },
    /// Delete a user including all API keys, uploads and files
    Delete {
        username: String,
        /// Required, as this cannot be undone
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Create an API key for a user
    Create {
        username: String,
        #[arg(long, default_value = "cli")]
        name: String,
    },
    /// List API keys of a user
    List { username: String },
    /// Revoke an API key by ID
    Revoke { key_id: String },
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let g = &cli.global;
    let db = db::connect(&g.database_url, g.db_max_connections)
        .await
        .context("connecting to database")?;
    if !g.skip_migrations || matches!(cli.command, Command::Migrate) {
        db::migrate(&db).await.context("running migrations")?;
    }
    let storage = Storage::open(&g.storage_dir)
        .await
        .with_context(|| format!("opening storage dir {}", g.storage_dir.display()))?;
    let ctx = Ctx {
        db,
        storage,
        public_url: g.public_url.trim_end_matches('/').to_string(),
    };

    match cli.command {
        Command::Serve(opts) => serve(ctx, opts).await,
        Command::Migrate => {
            println!("migrations applied");
            Ok(())
        }
        Command::User(cmd) => user_cmd(&ctx, cmd).await,
        Command::Key(cmd) => key_cmd(&ctx, cmd).await,
        Command::Import(opts) => crate::import::run(&ctx, opts).await,
    }
}

async fn serve(ctx: Ctx, opts: ServeOpts) -> anyhow::Result<()> {
    let config = ApiConfig {
        max_upload_bytes: usize::try_from(opts.max_upload_size.0).context("max upload size too large")?,
        cors_origins: opts
            .cors_origins
            .into_iter()
            .map(|o| o.trim().to_string())
            .filter(|o| !o.is_empty())
            .collect(),
        metrics_token: opts.metrics_token.filter(|t| !t.is_empty()),
    };
    tracing::info!(
        bind = %opts.bind,
        public_url = %ctx.public_url,
        storage = %ctx.storage.root().display(),
        "starting server"
    );
    let app = api::router(ctx, config);
    let listener = tokio::net::TcpListener::bind(opts.bind)
        .await
        .with_context(|| format!("binding {}", opts.bind))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutting down");
}

fn limit_to_db<T: Into<u64>>(limit: Option<T>) -> anyhow::Result<Option<i64>> {
    limit
        .map(|v| i64::try_from(v.into()).context("quota too large"))
        .transpose()
}

pub async fn require_user(ctx: &Ctx, username: &str) -> anyhow::Result<User> {
    db::get_user_by_name(&ctx.db, username)
        .await?
        .with_context(|| format!("user {username:?} not found"))
}

fn fmt_limit(limit: Option<i64>, bytes: bool) -> String {
    match limit {
        None => "unlimited".into(),
        Some(v) if bytes => ByteSize(v as u64).to_string(),
        Some(v) => v.to_string(),
    }
}

async fn user_cmd(ctx: &Ctx, cmd: UserCmd) -> anyhow::Result<()> {
    match cmd {
        UserCmd::Create {
            username,
            admin,
            quota_bytes,
            quota_uploads,
            no_key,
        } => {
            let username = validate_username(&username).map_err(anyhow::Error::msg)?;
            let new = NewUser {
                username,
                role: if admin { ROLE_ADMIN } else { ROLE_USER },
                quota_bytes: limit_to_db(quota_bytes.0)?,
                quota_uploads: limit_to_db(quota_uploads.0)?,
            };
            let user = match db::create_user(&ctx.db, new).await {
                Err(e) if db::is_unique_violation(&e) => bail!("user {username:?} already exists"),
                other => other?,
            };
            println!("created user {} ({}, id {})", user.username, user.role, user.id);
            if !no_key {
                let (key, token) = db::create_api_key(&ctx.db, &user.id, "initial").await?;
                println!("API key {} (shown only once):\n{token}", key.id);
            }
        }
        UserCmd::List => {
            let users = db::list_users(&ctx.db).await?;
            println!(
                "{:<24} {:<6} {:>10} {:>14} {:>8} {:>10}",
                "USERNAME", "ROLE", "USED", "QUOTA", "UPLOADS", "MAX"
            );
            for u in users {
                println!(
                    "{:<24} {:<6} {:>10} {:>14} {:>8} {:>10}",
                    u.username,
                    u.role,
                    ByteSize(u.used_bytes as u64).to_string(),
                    fmt_limit(u.quota_bytes, true),
                    u.upload_count,
                    fmt_limit(u.quota_uploads, false),
                );
            }
        }
        UserCmd::Update {
            username,
            role,
            quota_bytes,
            quota_uploads,
        } => {
            let user = require_user(ctx, &username).await?;
            let update = UserUpdate {
                role,
                quota_bytes: quota_bytes.map(|l| limit_to_db(l.0)).transpose()?,
                quota_uploads: quota_uploads.map(|l| limit_to_db(l.0)).transpose()?,
            };
            let user = db::update_user(&ctx.db, &user.id, update)
                .await?
                .context("user vanished")?;
            println!(
                "{}: role {}, quota {}, max uploads {}",
                user.username,
                user.role,
                fmt_limit(user.quota_bytes, true),
                fmt_limit(user.quota_uploads, false)
            );
        }
        UserCmd::Delete { username, yes } => {
            let user = require_user(ctx, &username).await?;
            if !yes {
                bail!(
                    "this deletes {} with {} uploads ({}); pass --yes to confirm",
                    user.username,
                    user.upload_count,
                    ByteSize(user.used_bytes as u64)
                );
            }
            ctx.delete_user(&user.id).await?;
            println!("deleted user {}", user.username);
        }
    }
    Ok(())
}

async fn key_cmd(ctx: &Ctx, cmd: KeyCmd) -> anyhow::Result<()> {
    match cmd {
        KeyCmd::Create { username, name } => {
            let user = require_user(ctx, &username).await?;
            let (key, token) = db::create_api_key(&ctx.db, &user.id, &name).await?;
            println!(
                "API key {} for {} (shown only once):\n{token}",
                key.id, user.username
            );
        }
        KeyCmd::List { username } => {
            let user = require_user(ctx, &username).await?;
            println!("{:<36} {:<20} {:<14} {:<8}", "ID", "NAME", "PREFIX", "STATUS");
            for k in db::list_api_keys(&ctx.db, &user.id).await? {
                let status = if k.revoked_at.is_some() {
                    "revoked"
                } else {
                    "active"
                };
                println!("{:<36} {:<20} {:<14} {:<8}", k.id, k.name, k.prefix, status);
            }
        }
        KeyCmd::Revoke { key_id } => {
            if !db::revoke_api_key(&ctx.db, &key_id, None).await? {
                bail!("no active key with ID {key_id}");
            }
            println!("revoked key {key_id}");
        }
    }
    Ok(())
}
