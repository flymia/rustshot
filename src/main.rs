use std::io::IsTerminal;

use clap::Parser;
use rustshot::cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(cli.log_filter())
        .with_writer(std::io::stdout)
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    rustshot::cli::run(cli).await
}
