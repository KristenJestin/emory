use std::path::PathBuf;

use anyhow::Context;
use voice::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "voice=info".into()),
        )
        .init();
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: voice <configuration.toml>")?;
    voice::run(Config::load(&path)?).await
}
