//! `mind <configuration.toml>`: the mind of Emory.

use std::path::PathBuf;

use anyhow::Result;
use mind::config::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "mind=info".into()))
        .init();
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: mind <configuration.toml>");
        std::process::exit(2);
    };
    mind::run(Config::read(&path)?).await
}
