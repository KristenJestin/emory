//! Tells whether the web search of `mind` works right now: one search of a neutral question
//! through the provider of `[web.search]`, and what became of it. DuckDuckGo's light page is read
//! with no key and changes without notice: this is what the owner (or a system timer) runs to
//! know, and to know which case it is, before Emory tells it by voice.
//!
//! ```sh
//! cargo run -p mind --example web-check                       # the default configuration
//! cargo run -p mind --example web-check -- --config mind.toml # the `[web.search]` of this file
//! cargo run -p mind --example web-check -- "another question"
//! ```
//!
//! One line on the standard output, and an exit code that tells the case:
//!
//! | Exit code | Line | Meaning |
//! |---|---|---|
//! | 0 | `ok: 5 results…` | the search works |
//! | 1 | `blocked…` | the provider took the program for a robot (a captcha) |
//! | 2 | `page changed…` | the page is not the one `mind` reads: the provider has to be fixed |
//! | 3 | `unreachable…` | the network, a time-out, a server error |
//! | 4 | `no result…` | the page works but the question found nothing (try another) |
//! | 5 | anything else | the configuration cannot be read, no provider |
//!
//! It makes one request, with no cache and no pause: do not run it in a loop, a provider that
//! sees too many requests blocks the program (and so the search of Emory).

use std::{process::ExitCode, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use mind::web::{
    Http, WebConfig,
    search::{DuckDuckGo, SearchError, SearchProvider},
};

/// A question nobody minds being asked.
const QUESTION: &str = "rust programming language";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            println!("error: {e:#}");
            ExitCode::from(5)
        }
    }
}

async fn run() -> Result<u8> {
    let mut config = WebConfig::default();
    let mut question = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let path = args.next().context("--config needs a file")?;
                config = read_config(&path)?;
            }
            _ => question = Some(arg),
        }
    }
    let search = &config.search;
    let provider: Box<dyn SearchProvider> = match search.provider.as_str() {
        "duckduckgo" => Box::new(DuckDuckGo::new(search, Arc::new(Http::for_search(search)?))),
        other => anyhow::bail!("the provider \"{other}\" has no search to check"),
    };
    let question = question.unwrap_or_else(|| QUESTION.to_owned());
    let started = Instant::now();
    let outcome = provider.search(&question, search.max_results).await;
    let ms = started.elapsed().as_millis();
    let name = provider.name();
    Ok(match outcome {
        Ok(hits) if hits.is_empty() => {
            println!("no result: {name} answers, and this question found nothing ({ms} ms)");
            4
        }
        Ok(hits) => {
            println!("ok: {} results from {name} ({ms} ms)", hits.len());
            0
        }
        Err(SearchError::Blocked(status)) => {
            println!("blocked: {name} answered HTTP {status}, a captcha or a refusal ({ms} ms)");
            1
        }
        Err(SearchError::PageChanged(status)) => {
            println!(
                "page changed: HTTP {status} from {name}, without the results `mind` reads ({ms} ms)"
            );
            2
        }
        Err(SearchError::Unreachable(status)) => {
            let status = status.map_or("no answer".to_owned(), |s| format!("HTTP {s}"));
            println!("unreachable: {name}, {status} ({ms} ms)");
            3
        }
        Err(SearchError::Refused(why)) => {
            println!("refused: {why}");
            5
        }
    })
}

/// The `[web]` section of a configuration file of `mind`, checked as `mind` does at its start.
fn read_config(path: &str) -> Result<WebConfig> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut file: toml::Table = text.parse().with_context(|| format!("parsing {path}"))?;
    let config = match file.remove("web") {
        Some(web) => web.try_into().context("the [web] section")?,
        None => WebConfig::default(),
    };
    config.check()?;
    Ok(config)
}
