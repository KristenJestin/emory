use std::path::PathBuf;

use anyhow::Context;
use bouche::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bouche=info".into()),
        )
        .init();
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage : bouche <configuration.toml>")?;
    bouche::run(Config::load(&path)?).await
}
