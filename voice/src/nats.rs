//! The connection to the bus: TLS and key (nkey) when the configuration gives them, nothing else.

use anyhow::Context;

use crate::config::Config;

pub async fn connect(config: &Config) -> anyhow::Result<async_nats::Client> {
    connect_to(
        &config.nats_url,
        config.nats_ca.as_deref(),
        config.nats_nkey_seed_file.as_deref(),
        "voice",
    )
    .await
}

pub async fn connect_to(
    url: &str,
    ca: Option<&std::path::Path>,
    nkey_seed_file: Option<&std::path::Path>,
    name: &str,
) -> anyhow::Result<async_nats::Client> {
    let mut options = async_nats::ConnectOptions::new().name(name);
    if let Some(ca) = ca {
        options = options
            .require_tls(true)
            .add_root_certificates(ca.to_owned());
    }
    if let Some(file) = nkey_seed_file {
        let seed = std::fs::read_to_string(file)
            .with_context(|| format!("reading the seed {}", file.display()))?;
        options = options.nkey(seed.trim().to_owned());
    }
    options
        .connect(url)
        .await
        .with_context(|| format!("connecting to NATS {url}"))
}
