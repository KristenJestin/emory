//! Emory speaking on her own, with no turn of the user: what the timers and the internal entry ask
//! for (see `specs/events.md`, « La parole d'elle-même »).
//!
//! The code that wants something said holds a [`Speaker`] and sends a [`Speech`]; the run loop
//! hands it to the [`Mind`](crate::mind::Mind) as an [`Input::Speak`](crate::mind::Input::Speak).
//! The mind waits until the device is free (no answer running or being said, no button held, no
//! turn being heard), then says the text like an answer, in a turn of its own. Never reached by the
//! voice nor by the LLM: no tool and no bus subject leads here.

use std::{fmt, time::Duration};

use contract::Device;
use tokio::sync::mpsc;

/// How long a speech waits for its device by default before it is dropped.
pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(600);

/// Who asked for the speech, for the logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A timer or a reminder that rings.
    Timer,
    /// The internal entry (`mind.event.<source>`).
    Event,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Timer => "timer",
            Self::Event => "event",
        })
    }
}

/// A text to say on a device, on Emory's own.
#[derive(Clone, Debug)]
pub struct Speech {
    pub device: Device,
    /// Said as it is, cut into sentences; never logged.
    pub text: String,
    pub origin: Origin,
    /// A short tone first (`Cue` `tone`), so that the owner looks up.
    pub chime: bool,
    /// Dropped, and logged, if the device is not free within this time.
    pub expires_in: Duration,
}

impl Speech {
    pub fn new(device: Device, text: impl Into<String>, origin: Origin) -> Self {
        Self {
            device,
            text: text.into(),
            origin,
            chime: true,
            expires_in: DEFAULT_EXPIRY,
        }
    }

    pub fn chime(mut self, chime: bool) -> Self {
        self.chime = chime;
        self
    }

    pub fn expires_in(mut self, expires_in: Duration) -> Self {
        self.expires_in = expires_in;
        self
    }
}

/// Asks the mind to speak. Cheap to clone: one per timer task, one for the internal entry.
#[derive(Clone)]
pub struct Speaker(mpsc::UnboundedSender<Speech>);

impl Speaker {
    /// Queues the speech; `false` if the mind has stopped. It is said later, when the device is
    /// free, or dropped at its expiry.
    pub fn speak(&self, speech: Speech) -> bool {
        self.0.send(speech).is_ok()
    }
}

/// The speeches asked for, for the run loop to hand to the mind.
pub type Speeches = mpsc::UnboundedReceiver<Speech>;

pub fn channel() -> (Speaker, Speeches) {
    let (sender, speeches) = mpsc::unbounded_channel();
    (Speaker(sender), speeches)
}
