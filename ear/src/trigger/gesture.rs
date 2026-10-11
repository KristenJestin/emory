//! Tap or hold, like Kris's dictation key: the pure state machine behind the `shortcut` trigger.
//!
//! A press opens a turn at once, so that no word is lost. If the key is let go before the
//! threshold it was a tap: the turn stays open and the next tap closes it. If it is let go after
//! the threshold it was a hold: the turn closes on release.
//!
//! A release that never comes (a bind without its release half) must not jam the key: a press
//! long after the previous one, with no release seen, is taken as if the key had been let go
//! at once, so that the key still works as a plain toggle.

use std::time::{Duration, Instant};

/// A press this long after the previous one, with no release between, means the release was lost.
/// (A held key does not repeat its press unless the bind asks for it, and a repeat comes much
/// faster than this.)
const LOST_RELEASE: Duration = Duration::from_secs(1);

/// What the gesture asks of the ear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Open,
    Close,
}

#[derive(Clone, Copy, Debug)]
enum State {
    /// No turn.
    Idle,
    /// A turn opened by a press, the key still down.
    Pressed(Instant),
    /// A turn left open by a tap, the key up.
    Open,
    /// The key is down again to close the turn; waiting for it to be let go.
    Closing,
}

#[derive(Clone, Debug)]
pub struct Gesture {
    threshold: Duration,
    state: State,
    last_press: Option<Instant>,
}

impl Gesture {
    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            state: State::Idle,
            last_press: None,
        }
    }

    pub fn pressed(&mut self, at: Instant) -> Option<Action> {
        let lost = self
            .last_press
            .is_some_and(|last| at.saturating_duration_since(last) >= LOST_RELEASE);
        self.last_press = Some(at);
        if lost {
            match self.state {
                State::Pressed(_) => self.state = State::Open,
                State::Closing => self.state = State::Idle,
                State::Idle | State::Open => {}
            }
        }
        match self.state {
            State::Idle => {
                self.state = State::Pressed(at);
                Some(Action::Open)
            }
            State::Open => {
                self.state = State::Closing;
                Some(Action::Close)
            }
            // A repeated press while the key is already down.
            State::Pressed(_) | State::Closing => None,
        }
    }

    pub fn released(&mut self, at: Instant) -> Option<Action> {
        match self.state {
            State::Pressed(since) if at.saturating_duration_since(since) >= self.threshold => {
                self.state = State::Idle;
                Some(Action::Close)
            }
            State::Pressed(_) => {
                self.state = State::Open;
                None
            }
            State::Closing => {
                self.state = State::Idle;
                None
            }
            State::Idle | State::Open => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: Duration = Duration::from_millis(300);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_tap_opens_and_the_next_tap_closes() {
        let t0 = Instant::now();
        let mut g = Gesture::new(THRESHOLD);
        assert_eq!(g.pressed(t0), Some(Action::Open));
        assert_eq!(g.released(t0 + ms(120)), None);
        // Long after: still open.
        assert_eq!(g.pressed(t0 + ms(9000)), Some(Action::Close));
        assert_eq!(g.released(t0 + ms(9100)), None);
        // And the cycle starts again.
        assert_eq!(g.pressed(t0 + ms(12000)), Some(Action::Open));
    }

    #[test]
    fn a_hold_closes_on_release() {
        let t0 = Instant::now();
        let mut g = Gesture::new(THRESHOLD);
        assert_eq!(g.pressed(t0), Some(Action::Open));
        assert_eq!(g.released(t0 + ms(2500)), Some(Action::Close));
        assert_eq!(g.pressed(t0 + ms(3000)), Some(Action::Open));
    }

    #[test]
    fn the_threshold_itself_is_a_hold() {
        let t0 = Instant::now();
        let mut g = Gesture::new(THRESHOLD);
        g.pressed(t0);
        assert_eq!(g.released(t0 + THRESHOLD), Some(Action::Close));

        let mut g = Gesture::new(THRESHOLD);
        g.pressed(t0);
        assert_eq!(g.released(t0 + THRESHOLD - ms(1)), None);
    }

    #[test]
    fn a_long_press_to_close_does_not_reopen() {
        let t0 = Instant::now();
        let mut g = Gesture::new(THRESHOLD);
        g.pressed(t0);
        g.released(t0 + ms(100));
        assert_eq!(g.pressed(t0 + ms(5000)), Some(Action::Close));
        // Held for a long time, the second press is no hold of a new turn.
        assert_eq!(g.released(t0 + ms(8000)), None);
        assert_eq!(g.pressed(t0 + ms(9000)), Some(Action::Open));
    }

    #[test]
    fn a_bind_without_release_still_toggles() {
        let t0 = Instant::now();
        let mut g = Gesture::new(THRESHOLD);
        assert_eq!(g.pressed(t0), Some(Action::Open));
        assert_eq!(g.pressed(t0 + ms(4000)), Some(Action::Close));
        assert_eq!(g.pressed(t0 + ms(9000)), Some(Action::Open));
        assert_eq!(g.pressed(t0 + ms(12000)), Some(Action::Close));
    }

    #[test]
    fn fast_repeats_while_held_are_not_a_lost_release() {
        let t0 = Instant::now();
        let mut g = Gesture::new(THRESHOLD);
        assert_eq!(g.pressed(t0), Some(Action::Open));
        // The key is held for 3 s and repeats every 30 ms from 600 ms on.
        for i in 0..80 {
            assert_eq!(g.pressed(t0 + ms(600 + 30 * i)), None);
        }
        assert_eq!(g.released(t0 + ms(3000)), Some(Action::Close));
    }

    #[test]
    fn repeats_and_stray_releases_are_ignored() {
        let t0 = Instant::now();
        let mut g = Gesture::new(THRESHOLD);
        assert_eq!(g.released(t0), None);
        assert_eq!(g.pressed(t0), Some(Action::Open));
        assert_eq!(g.pressed(t0 + ms(30)), None);
        assert_eq!(g.pressed(t0 + ms(60)), None);
        assert_eq!(g.released(t0 + ms(100)), None);
        assert_eq!(g.released(t0 + ms(110)), None);
        assert_eq!(g.pressed(t0 + ms(500)), Some(Action::Close));
    }
}
