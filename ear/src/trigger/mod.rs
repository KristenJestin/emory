//! What opens and closes a turn. A trigger is a module: `shortcut` on a PC, `button` on a
//! Raspberry and `continuous` for a room microphone come later and slot in here.

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;

use crate::config::{EarConfig, TriggerKind};

// The gesture of a key (tap or hold) belongs to the keyboard trigger.
#[cfg(feature = "shortcut")]
mod gesture;
#[cfg(feature = "shortcut")]
mod shortcut;

/// What a trigger asks of the ear.
#[cfg_attr(not(feature = "shortcut"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open a turn.
    Open,
    /// Close the turn.
    Close,
}

pub trait Trigger: Send + 'static {
    /// The name of the module, as announced on the bus.
    fn name(&self) -> &'static str;

    /// Sends commands until it fails or the ear no longer listens.
    fn run(self: Box<Self>, commands: mpsc::Sender<Command>) -> BoxFuture<'static, Result<()>>;
}

/// The trigger of the configuration.
pub fn build(config: &EarConfig) -> Result<Box<dyn Trigger>> {
    match config.trigger {
        #[cfg(feature = "shortcut")]
        TriggerKind::Shortcut => Ok(Box::new(shortcut::Shortcut::new(config))),
        #[cfg(not(feature = "shortcut"))]
        TriggerKind::Shortcut => bail!("this build of emory-ear has no `shortcut` trigger"),
        other => bail!("the `{}` trigger does not exist yet", other.name()),
    }
}
