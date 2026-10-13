//! What the time module writes in the logs: the id, the kind, the due time and the device, never
//! the label or the text. Alone in its file: a subscriber set for one test sees the events of the
//! others' callsites only if no other test of the process has already hit them without one.

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use chrono::Utc;
use mind::{
    conversations::Conversations,
    time::{ManualClock, Time, TimeConfig},
    tools::Module,
};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn call(time: &Time, tool: &str, arguments: Value) {
    let result = time.call(tool, arguments.as_object().cloned()).await;
    assert_ne!(result.is_error, Some(true), "{tool}: {result:?}");
}

#[tokio::test]
async fn the_logs_never_hold_the_label_or_the_text() {
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let config = TimeConfig {
        timezone: "Europe/Paris".to_owned(),
        ..TimeConfig::default()
    };
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let time = Time::with_clock(
        &config,
        &dir.path().join("timers.json"),
        clock,
        Conversations::default(),
    )
    .unwrap();
    call(
        &time,
        "timer",
        json!({"seconds": 60, "label": "secretlabel"}),
    )
    .await;
    call(
        &time,
        "reminder",
        json!({"text": "secrettext", "in_seconds": 60}),
    )
    .await;
    call(&time, "list", json!({})).await;
    call(&time, "cancel", json!({"id": 1})).await;

    let logs = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("timer or reminder set"), "{logs}");
    assert!(logs.contains("timer or reminder cancelled"), "{logs}");
    assert!(!logs.contains("secret"), "{logs}");
}
