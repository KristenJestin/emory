//! La bouche d'Emory : reçoit les phrases à dire (`bouche.dire`), les fait synthétiser par le
//! moteur de voix et renvoie le son au poste (`poste.<nom>.audio.retour`). Voir `specs/bouche.md`.

pub mod audio;
pub mod config;
pub mod engine;
pub mod nats;
pub mod poste;
pub mod queue;
pub mod text;

use std::collections::HashMap;

use contract::{Appareil, Dire, Stop, sujets};
use futures_util::StreamExt;
use tracing::{info, warn};

use config::Config;
use poste::Input;

/// Fait tourner la bouche jusqu'à l'arrêt du programme (Ctrl-C ou SIGTERM).
pub async fn run(config: Config) -> anyhow::Result<()> {
    let nats = nats::connect(&config).await?;
    let engine = engine::Engine::new(&config.engine_url, &config.voice)?;
    match engine.warm_up().await {
        Ok(()) => info!(engine = %config.engine_url, voice = %config.voice, "moteur prêt"),
        Err(e) => warn!("moteur pas encore prêt, la première phrase sera lente ou perdue : {e:#}"),
    }
    let mut dire = nats.subscribe(sujets::DIRE).await?;
    let mut stop = nats.subscribe(sujets::STOP).await?;
    info!(nats = %config.nats_url, "la bouche écoute");

    let mut postes: HashMap<Appareil, (poste::Inbox, tokio::task::JoinHandle<()>)> = HashMap::new();
    let mut inbox_of = |name: &Appareil| {
        postes
            .entry(name.clone())
            .or_insert_with(|| {
                poste::spawn(name.clone(), nats.clone(), engine.clone(), config.clone())
            })
            .0
            .clone()
    };
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        tokio::select! {
            Some(message) = dire.next() => match serde_json::from_slice::<Dire>(&message.payload) {
                Ok(d) => { let _ = inbox_of(&d.poste).send(Input::Dire(d)); }
                Err(e) => warn!("Dire illisible : {e}"),
            },
            Some(message) = stop.next() => match serde_json::from_slice::<Stop>(&message.payload) {
                Ok(s) => { let _ = inbox_of(&s.poste).send(Input::Stop(s.tour)); }
                Err(e) => warn!("Stop illisible : {e}"),
            },
            () = &mut shutdown => break,
            else => break,
        }
    }
    info!("arrêt");
    for (inbox, handle) in postes.into_values() {
        let _ = inbox.send(Input::Shutdown);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
    nats.flush().await.ok();
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
