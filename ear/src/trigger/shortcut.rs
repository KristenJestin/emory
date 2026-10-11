//! The `shortcut` trigger: the GlobalShortcuts portal of xdg-desktop-portal (on Hyprland,
//! xdg-desktop-portal-hyprland). One shortcut, `talk`, which the compositor's configuration binds
//! to a key (see the README): tap to open and tap to close, or hold to talk.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ashpd::{
    AppID,
    desktop::global_shortcuts::{GlobalShortcuts, NewShortcut},
};
use futures_util::{StreamExt, future::BoxFuture};
use tokio::sync::mpsc;

use super::{
    Command, Trigger,
    gesture::{Action, Gesture},
};
use crate::config::EarConfig;

/// The identity of the program for the portal: the first half of the name a compositor binds
/// (`dev.emory.ear:talk`).
pub const APP_ID: &str = "dev.emory.ear";
pub const SHORTCUT_ID: &str = "talk";
const DESCRIPTION: &str = "Talk to Emory";

pub struct Shortcut {
    hold_threshold: Duration,
}

impl Shortcut {
    pub fn new(config: &EarConfig) -> Self {
        Self {
            hold_threshold: Duration::from_millis(config.hold_threshold_ms),
        }
    }
}

impl Trigger for Shortcut {
    fn name(&self) -> &'static str {
        "shortcut"
    }

    fn run(self: Box<Self>, commands: mpsc::Sender<Command>) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move { self.listen(commands).await })
    }
}

impl Shortcut {
    async fn listen(&self, commands: mpsc::Sender<Command>) -> Result<()> {
        // A program outside a sandbox has no identity for the portal until it gives one.
        match AppID::try_from(APP_ID) {
            Ok(id) => {
                if let Err(error) = ashpd::register_host_app(id).await {
                    tracing::warn!(%error, "could not register the app id with the portal");
                }
            }
            Err(error) => tracing::warn!(%error, "invalid app id"),
        }
        let portal = GlobalShortcuts::new()
            .await
            .context("the GlobalShortcuts portal is not available (xdg-desktop-portal and a backend for the compositor are needed)")?;
        let session = portal
            .create_session(Default::default())
            .await
            .context("creating the shortcuts session")?;
        // Subscribed before the binding, so that no press is missed.
        let mut activated = portal.receive_activated().await?;
        let mut deactivated = portal.receive_deactivated().await?;
        let bound = portal
            .bind_shortcuts(
                &session,
                &[NewShortcut::new(SHORTCUT_ID, DESCRIPTION)],
                None,
                Default::default(),
            )
            .await
            .context("binding the shortcut")?
            .response()
            .context("the portal refused the shortcut")?;
        for shortcut in bound.shortcuts() {
            tracing::info!(
                id = shortcut.id(),
                trigger = shortcut.trigger_description(),
                "shortcut registered with the portal"
            );
        }

        let mut gesture = Gesture::new(self.hold_threshold);
        loop {
            let action = tokio::select! {
                Some(event) = activated.next() => {
                    (event.shortcut_id() == SHORTCUT_ID).then(|| gesture.pressed(Instant::now())).flatten()
                }
                Some(event) = deactivated.next() => {
                    (event.shortcut_id() == SHORTCUT_ID).then(|| gesture.released(Instant::now())).flatten()
                }
                else => anyhow::bail!("the portal stopped sending shortcut events"),
            };
            let command = match action {
                Some(Action::Open) => Command::Open,
                Some(Action::Close) => Command::Close,
                None => continue,
            };
            if commands.send(command).await.is_err() {
                return Ok(());
            }
        }
    }
}
