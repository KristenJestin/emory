//! The state machine of the turns: at most one open turn per device, from the `start` to the
//! `end`.
//!
//! It knows neither the bus nor the models: it follows the messages (start, numbered audio chunks,
//! end, time passing) and says which turn opens, receives audio or closes. Each turn carries a
//! `session` that the caller builds, and that it hands back on closing.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use contract::{Device, Timestamp, TurnId};

/// Why a turn closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The second press.
    End,
    /// A new `start` on a device whose turn was still open.
    Replaced,
    /// No audio for [`contract::audio::SILENCE_CLOSES_TURN`]: the ear is dead.
    Silence,
    /// The program is stopping.
    Shutdown,
}

/// A gap in the audio numbering: `expected..received` never arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    pub expected: u32,
    pub received: u32,
}

/// A closed turn, handed back with its session.
#[derive(Debug)]
pub struct Closed<S> {
    pub device: Device,
    pub turn: TurnId,
    pub start: Timestamp,
    pub reason: Reason,
    pub session: S,
}

/// What happens to a `start`.
#[derive(Debug)]
pub enum Opening<S> {
    /// The turn is open; `replaced` is the one it closed on this device, if there was one.
    Opened { replaced: Option<Closed<S>> },
    /// This turn is already open (message received twice): nothing changes.
    Duplicate,
}

/// What happens to a chunk of audio.
#[derive(Debug)]
pub enum Chunk<'a, S> {
    /// No open turn for this device and this identifier: the audio is thrown away.
    Unknown,
    /// A chunk already seen, or late compared with those that followed: it is thrown away.
    Stale,
    /// The chunk is for the session; `gap` says what was missing before it.
    Accepted {
        session: &'a mut S,
        gap: Option<Gap>,
    },
}

struct Open<S> {
    turn: TurnId,
    start: Timestamp,
    next_seq: u32,
    last_audio: Instant,
    session: S,
}

pub struct Turns<S> {
    open: HashMap<Device, Open<S>>,
    silence: Duration,
}

impl<S> Turns<S> {
    pub fn new(silence: Duration) -> Self {
        Self {
            open: HashMap::new(),
            silence,
        }
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// The device starts a turn. The silence is counted from now.
    pub fn start(
        &mut self,
        device: &Device,
        turn: TurnId,
        at: Timestamp,
        now: Instant,
        create: impl FnOnce() -> S,
    ) -> Opening<S> {
        if self.open.get(device).is_some_and(|o| o.turn == turn) {
            return Opening::Duplicate;
        }
        let replaced = self.close(device, Reason::Replaced);
        self.open.insert(
            device.clone(),
            Open {
                turn,
                start: at,
                next_seq: 0,
                last_audio: now,
                session: create(),
            },
        );
        Opening::Opened { replaced }
    }

    /// A chunk of audio numbered `seq` for the turn `turn` of the device.
    pub fn audio(&mut self, device: &Device, turn: TurnId, seq: u32, now: Instant) -> Chunk<'_, S> {
        let Some(open) = self.open.get_mut(device).filter(|o| o.turn == turn) else {
            return Chunk::Unknown;
        };
        if seq < open.next_seq {
            return Chunk::Stale;
        }
        let gap = (seq > open.next_seq).then_some(Gap {
            expected: open.next_seq,
            received: seq,
        });
        open.next_seq = seq.saturating_add(1);
        open.last_audio = now;
        Chunk::Accepted {
            session: &mut open.session,
            gap,
        }
    }

    /// The second press. `None` when this turn is not (or no longer) open: it may have been closed
    /// by the silence or replaced.
    pub fn end(&mut self, device: &Device, turn: TurnId) -> Option<Closed<S>> {
        if self.open.get(device)?.turn != turn {
            return None;
        }
        self.close(device, Reason::End)
    }

    /// Closes the turns that have received no audio for long enough.
    pub fn expire(&mut self, now: Instant) -> Vec<Closed<S>> {
        let expired: Vec<Device> = self
            .open
            .iter()
            .filter(|(_, o)| now.saturating_duration_since(o.last_audio) >= self.silence)
            .map(|(device, _)| device.clone())
            .collect();
        expired
            .iter()
            .filter_map(|device| self.close(device, Reason::Silence))
            .collect()
    }

    /// Closes everything, when the program stops.
    pub fn close_all(&mut self) -> Vec<Closed<S>> {
        let devices: Vec<Device> = self.open.keys().cloned().collect();
        devices
            .iter()
            .filter_map(|device| self.close(device, Reason::Shutdown))
            .collect()
    }

    fn close(&mut self, device: &Device, reason: Reason) -> Option<Closed<S>> {
        let open = self.open.remove(device)?;
        Some(Closed {
            device: device.clone(),
            turn: open.turn,
            start: open.start,
            reason,
            session: open.session,
        })
    }
}

#[cfg(test)]
mod tests {
    use contract::audio::SILENCE_CLOSES_TURN;

    use super::*;

    fn device(name: &str) -> Device {
        name.parse().unwrap()
    }

    fn machine() -> Turns<Vec<u32>> {
        Turns::new(SILENCE_CLOSES_TURN)
    }

    fn open(m: &mut Turns<Vec<u32>>, d: &Device, t: TurnId, now: Instant) -> Opening<Vec<u32>> {
        m.start(d, t, contract::now(), now, Vec::new)
    }

    fn feed(m: &mut Turns<Vec<u32>>, d: &Device, t: TurnId, seq: u32, now: Instant) {
        match m.audio(d, t, seq, now) {
            Chunk::Accepted { session, .. } => session.push(seq),
            other => panic!("audio refused: {other:?}"),
        }
    }

    #[test]
    fn a_turn_from_audio_to_end() {
        let (mut m, d, t, now) = (machine(), device("office"), TurnId::new(), Instant::now());
        assert!(matches!(
            open(&mut m, &d, t, now),
            Opening::Opened { replaced: None }
        ));
        for seq in 0..3 {
            feed(&mut m, &d, t, seq, now);
        }
        let closed = m.end(&d, t).unwrap();
        assert_eq!((closed.reason, closed.turn), (Reason::End, t));
        assert_eq!(closed.session, vec![0, 1, 2]);
        assert_eq!(m.open_count(), 0);
    }

    #[test]
    fn a_gap_is_reported_and_the_audio_goes_on() {
        let (mut m, d, t, now) = (machine(), device("office"), TurnId::new(), Instant::now());
        open(&mut m, &d, t, now);
        feed(&mut m, &d, t, 0, now);
        match m.audio(&d, t, 4, now) {
            Chunk::Accepted { gap, .. } => {
                assert_eq!(
                    gap,
                    Some(Gap {
                        expected: 1,
                        received: 4
                    })
                );
            }
            other => panic!("{other:?}"),
        }
        match m.audio(&d, t, 5, now) {
            Chunk::Accepted { gap, .. } => assert_eq!(gap, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_repeated_or_late_chunk_is_thrown_away() {
        let (mut m, d, t, now) = (machine(), device("office"), TurnId::new(), Instant::now());
        open(&mut m, &d, t, now);
        feed(&mut m, &d, t, 0, now);
        feed(&mut m, &d, t, 3, now);
        assert!(matches!(m.audio(&d, t, 3, now), Chunk::Stale));
        assert!(matches!(m.audio(&d, t, 2, now), Chunk::Stale));
        assert_eq!(m.end(&d, t).unwrap().session, vec![0, 3]);
    }

    #[test]
    fn the_audio_of_another_turn_or_without_a_turn_is_unknown() {
        let (mut m, d, t, now) = (machine(), device("office"), TurnId::new(), Instant::now());
        assert!(matches!(m.audio(&d, t, 0, now), Chunk::Unknown));
        open(&mut m, &d, t, now);
        assert!(matches!(m.audio(&d, TurnId::new(), 0, now), Chunk::Unknown));
        assert!(matches!(
            m.audio(&device("living"), t, 0, now),
            Chunk::Unknown
        ));
    }

    #[test]
    fn a_new_start_closes_the_old_turn_of_the_device() {
        let (mut m, d, now) = (machine(), device("office"), Instant::now());
        let (first, second) = (TurnId::new(), TurnId::new());
        open(&mut m, &d, first, now);
        feed(&mut m, &d, first, 0, now);
        let Opening::Opened { replaced } = open(&mut m, &d, second, now) else {
            panic!("the second turn should open");
        };
        let closed = replaced.expect("the old turn is closed");
        assert_eq!((closed.reason, closed.turn), (Reason::Replaced, first));
        assert_eq!(closed.session, vec![0]);
        // The numbering restarts at 0 for the new turn; the old one no longer listens.
        assert!(matches!(m.audio(&d, first, 1, now), Chunk::Unknown));
        feed(&mut m, &d, second, 0, now);
        // The end of the old turn, arriving late, does not close the new one.
        assert!(m.end(&d, first).is_none());
        assert_eq!(m.open_count(), 1);
    }

    #[test]
    fn a_repeated_start_changes_nothing() {
        let (mut m, d, t, now) = (machine(), device("office"), TurnId::new(), Instant::now());
        open(&mut m, &d, t, now);
        feed(&mut m, &d, t, 0, now);
        assert!(matches!(open(&mut m, &d, t, now), Opening::Duplicate));
        assert_eq!(m.end(&d, t).unwrap().session, vec![0]);
    }

    #[test]
    fn two_devices_each_have_their_own_turn() {
        let (mut m, now) = (machine(), Instant::now());
        let (office, living) = (device("office"), device("living"));
        let (t1, t2) = (TurnId::new(), TurnId::new());
        open(&mut m, &office, t1, now);
        assert!(matches!(
            open(&mut m, &living, t2, now),
            Opening::Opened { replaced: None }
        ));
        feed(&mut m, &office, t1, 0, now);
        feed(&mut m, &living, t2, 0, now);
        assert_eq!(m.end(&living, t2).unwrap().device, living);
        assert_eq!(m.open_count(), 1);
    }

    #[test]
    fn a_turn_without_audio_for_three_seconds_closes() {
        let (mut m, d, t, start) = (machine(), device("office"), TurnId::new(), Instant::now());
        open(&mut m, &d, t, start);
        feed(&mut m, &d, t, 0, start);
        let last = start + Duration::from_secs(5);
        feed(&mut m, &d, t, 1, last);
        assert!(m.expire(last + Duration::from_millis(2_999)).is_empty());
        let closed = m.expire(last + SILENCE_CLOSES_TURN);
        assert_eq!(closed.len(), 1);
        assert_eq!((closed[0].reason, closed[0].turn), (Reason::Silence, t));
        assert_eq!(closed[0].session, vec![0, 1]);
        // Closed: a late end, or audio that would resume, reopen nothing.
        assert!(m.end(&d, t).is_none());
        assert!(matches!(m.audio(&d, t, 2, last), Chunk::Unknown));
    }

    #[test]
    fn a_turn_that_never_had_audio_expires_too() {
        let (mut m, d, t, start) = (machine(), device("office"), TurnId::new(), Instant::now());
        open(&mut m, &d, t, start);
        assert!(m.expire(start + Duration::from_secs(1)).is_empty());
        assert_eq!(m.expire(start + Duration::from_secs(3)).len(), 1);
    }

    #[test]
    fn shutdown_closes_everything() {
        let (mut m, now) = (machine(), Instant::now());
        open(&mut m, &device("office"), TurnId::new(), now);
        open(&mut m, &device("living"), TurnId::new(), now);
        let closed = m.close_all();
        assert_eq!(closed.len(), 2);
        assert!(closed.iter().all(|c| c.reason == Reason::Shutdown));
        assert_eq!(m.open_count(), 0);
    }
}
