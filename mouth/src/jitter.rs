//! The jitter buffer: orders the chunks of a turn, decides when playback starts and which chunks
//! are dropped. Pure logic: time is given by the caller, nothing here touches the bus or the
//! audio hardware.
//!
//! - Chunks are ordered by `Emory-Seq` within a turn. A missing chunk is waited for a short time
//!   (`gap_wait`), then skipped, and logged.
//! - Playback starts once `start_buffer` of audio is ready, or a sentence ends, or the oldest
//!   ready chunk has waited `max_hold`. After an underrun (everything released has been played)
//!   the same rule applies again. While audio is still playing, ready chunks go straight on.
//! - The voice is faster than real time: nothing is ever dropped for being early, a whole answer
//!   is buffered.
//! - A turn id is a ULID, so turns are ordered. The newest turn seen wins: chunks of an older
//!   turn are dropped. A press of the button (`start`) stops everything and makes its turn the
//!   newest, so the end of an answer the server is still sending is dropped.
//! - A chunk of a newer turn than the one playing stops the old one exactly like a press (the
//!   press may have been lost, or the turn may come from another path than the button).
//! - Clock safety: if a chunk of an "older" turn arrives when nothing of the newest turn has
//!   been received for [`CLOCK_SILENCE`], the clock of the ear that makes the ids has probably
//!   gone back: the chunk is taken as a new turn instead of dropping everything for good.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use bytes::Bytes;
use contract::{TurnId, audio::Format};
use tracing::{debug, warn};

/// How long the newest turn must have been silent for an older turn to be taken as a new one.
pub const CLOCK_SILENCE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Audio to have ready before (re)starting to play.
    pub start_buffer: Duration,
    /// How long a missing chunk is waited for before it is skipped.
    pub gap_wait: Duration,
    /// The longest ready audio waits for `start_buffer` (a lost `Emory-End` must not mute it).
    pub max_hold: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_buffer: Duration::from_millis(60),
            gap_wait: Duration::from_millis(100),
            max_hold: Duration::from_millis(200),
        }
    }
}

/// A chunk of audio received from the bus.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub turn: TurnId,
    pub seq: u32,
    pub format: Format,
    pub sentence: u32,
    /// `Emory-End`: the last chunk of the sentence (it may carry no sound).
    pub end: bool,
    pub pcm: Bytes,
}

/// The first sound of a sentence: when its first chunk was received, to measure how long the
/// speaker took to play it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub turn: TurnId,
    pub sentence: u32,
    pub received: Instant,
}

/// Sound to give to the output, in order.
#[derive(Clone, Debug)]
pub struct Play {
    pub format: Format,
    pub pcm: Bytes,
    /// Set on the first sound of a sentence.
    pub mark: Option<Mark>,
}

impl Play {
    pub fn duration(&self) -> Duration {
        duration_of(self.format, self.pcm.len())
    }
}

/// Why the playback is stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// `Button` `start`.
    Button,
    /// A chunk of a newer turn arrived while an older one was playing.
    NewerTurn,
}

#[derive(Clone, Debug)]
pub enum Output {
    Play(Play),
    /// Stop playing at once and drop everything the output still holds.
    Flush(Cause),
}

pub fn duration_of(format: Format, bytes: usize) -> Duration {
    let frame = 2 * format.channels as usize;
    let frames = bytes / frame;
    Duration::from_secs_f64(frames as f64 / format.sample_rate as f64)
}

struct Reassembly {
    id: TurnId,
    next_seq: u32,
    pending: BTreeMap<u32, Chunk>,
    /// Since when `pending` is waiting for a missing chunk.
    gap_since: Option<Instant>,
    /// When the first chunk with sound of each sentence was received; `None` once the sentence
    /// has been marked (only its first sound carries a mark).
    first_received: BTreeMap<u32, Option<Instant>>,
}

pub struct Jitter {
    settings: Settings,
    newest: Option<TurnId>,
    /// When something of the newest turn was last received (or the button of it pressed).
    newest_seen: Option<Instant>,
    turn: Option<Reassembly>,
    /// In order, ready, waiting for the playback to (re)start.
    hold: Vec<Play>,
    hold_end: bool,
    hold_since: Option<Instant>,
    /// When everything released so far will have been played, if the output runs in real time.
    play_until: Option<Instant>,
}

impl Jitter {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            newest: None,
            newest_seen: None,
            turn: None,
            hold: Vec::new(),
            hold_end: false,
            hold_since: None,
            play_until: None,
        }
    }

    /// A chunk arrives.
    pub fn chunk(&mut self, chunk: Chunk, now: Instant) -> Vec<Output> {
        if let Some(newest) = self.newest
            && chunk.turn < newest
        {
            let quiet = self
                .newest_seen
                .is_none_or(|seen| now.duration_since(seen) >= CLOCK_SILENCE);
            if !quiet {
                debug!(turn = %chunk.turn, seq = chunk.seq, "chunk of an older turn dropped");
                return Vec::new();
            }
            warn!(
                turn = %chunk.turn,
                newest = %newest,
                "a turn older than the newest one comes after {CLOCK_SILENCE:?} of silence: \
                 the clock of the ear has probably gone back, taking it as a new turn",
            );
        }
        self.newest = Some(chunk.turn);
        self.newest_seen = Some(now);
        let mut outputs = Vec::new();
        if self.turn.as_ref().is_none_or(|t| t.id != chunk.turn) {
            if let Some(old) = &self.turn {
                if !old.pending.is_empty() {
                    warn!(turn = %old.id, chunks = old.pending.len(), "a newer turn starts: the chunks still missing in the older one are dropped");
                }
                // No press told us to stop: the new turn does.
                outputs.push(self.stop(Cause::NewerTurn));
            }
            self.turn = Some(Reassembly {
                id: chunk.turn,
                next_seq: 0,
                pending: BTreeMap::new(),
                gap_since: None,
                first_received: BTreeMap::new(),
            });
        }
        let mut turn = self.turn.take().expect("set just above");
        if chunk.seq < turn.next_seq {
            debug!(turn = %chunk.turn, seq = chunk.seq, expected = turn.next_seq, "late or repeated chunk dropped");
        } else {
            if !chunk.pcm.is_empty() {
                turn.first_received
                    .entry(chunk.sentence)
                    .or_insert(Some(now));
            }
            let seq = chunk.seq;
            turn.pending.insert(seq, chunk);
            if seq == turn.next_seq {
                self.drain(&mut turn, now);
            } else {
                turn.gap_since.get_or_insert(now);
            }
        }
        self.turn = Some(turn);
        outputs.extend(self.gate(now));
        outputs
    }

    /// The button is pressed (`Button` `start` of `turn`): stop at once, forget everything.
    pub fn press(&mut self, turn: TurnId, now: Instant) -> Vec<Output> {
        self.newest = Some(self.newest.map_or(turn, |newest| newest.max(turn)));
        self.newest_seen = Some(now);
        self.turn = None;
        vec![self.stop(Cause::Button)]
    }

    /// Forgets the audio waiting to start and the playback in progress.
    fn stop(&mut self, cause: Cause) -> Output {
        self.hold.clear();
        self.hold_end = false;
        self.hold_since = None;
        self.play_until = None;
        Output::Flush(cause)
    }

    /// Time passes: skip a chunk that did not come, start playing audio that waited too long.
    pub fn poll(&mut self, now: Instant) -> Vec<Output> {
        if let Some(mut turn) = self.turn.take() {
            if turn
                .gap_since
                .is_some_and(|since| now.duration_since(since) >= self.settings.gap_wait)
                && let Some(&next) = turn.pending.keys().next()
            {
                warn!(
                    turn = %turn.id,
                    "chunks {}..{} missing after {:?}: skipped",
                    turn.next_seq,
                    next - 1,
                    self.settings.gap_wait,
                );
                turn.next_seq = next;
                self.drain(&mut turn, now);
            }
            self.turn = Some(turn);
        }
        self.gate(now)
    }

    /// Moves the chunks that follow `next_seq` from `pending` to `hold`.
    fn drain(&mut self, turn: &mut Reassembly, now: Instant) {
        while let Some(chunk) = turn.pending.remove(&turn.next_seq) {
            // After u32::MAX chunks (years of speech) the numbering starts again at 0.
            turn.next_seq = turn.next_seq.checked_add(1).unwrap_or(0);
            if chunk.end {
                self.hold_end = true;
            }
            if chunk.pcm.is_empty() {
                continue;
            }
            let mark = turn
                .first_received
                .get_mut(&chunk.sentence)
                .and_then(Option::take)
                .map(|received| Mark {
                    turn: chunk.turn,
                    sentence: chunk.sentence,
                    received,
                });
            if self.hold.is_empty() {
                self.hold_since = Some(now);
            }
            self.hold.push(Play {
                format: chunk.format,
                pcm: chunk.pcm,
                mark,
            });
        }
        turn.gap_since = (!turn.pending.is_empty()).then_some(now);
    }

    /// Releases what is ready to the output when the rules say so.
    fn gate(&mut self, now: Instant) -> Vec<Output> {
        if self.hold.is_empty() {
            self.hold_end = false;
            self.hold_since = None;
            return Vec::new();
        }
        let playing = self.play_until.is_some_and(|until| until > now);
        let ready: Duration = self.hold.iter().map(Play::duration).sum();
        let waited = self
            .hold_since
            .map_or(Duration::ZERO, |since| now.duration_since(since));
        if !(playing
            || ready >= self.settings.start_buffer
            || self.hold_end
            || waited >= self.settings.max_hold)
        {
            return Vec::new();
        }
        let from = match self.play_until {
            Some(until) if playing => until,
            _ => now,
        };
        self.play_until = Some(from + ready);
        self.hold_end = false;
        self.hold_since = None;
        self.hold.drain(..).map(Output::Play).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONO_24K: Format = Format {
        sample_rate: 24_000,
        channels: 1,
    };

    fn turn_a() -> TurnId {
        "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
    }

    fn turn_b() -> TurnId {
        "01BX5ZZKBKACTAV9WEVGEMMVRZ".parse().unwrap()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A chunk of `n` milliseconds of sound at 24 kHz mono.
    fn chunk(turn: TurnId, seq: u32, sentence: u32, n: usize, end: bool) -> Chunk {
        Chunk {
            turn,
            seq,
            format: MONO_24K,
            sentence,
            end,
            pcm: Bytes::from(vec![(seq as u8).wrapping_add(1); n * 24 * 2]),
        }
    }

    fn jitter() -> (Jitter, Instant) {
        (Jitter::new(Settings::default()), Instant::now())
    }

    fn played(outputs: &[Output]) -> Vec<&Play> {
        outputs
            .iter()
            .filter_map(|o| match o {
                Output::Play(p) => Some(p),
                Output::Flush(_) => None,
            })
            .collect()
    }

    /// The first byte of each chunk tells which one it was (`seq + 1`).
    fn order(outputs: &[Output]) -> Vec<u8> {
        played(outputs).iter().map(|p| p.pcm[0]).collect()
    }

    #[test]
    fn waits_for_the_start_buffer_then_releases_in_order() {
        let (mut j, t) = jitter();
        assert!(j.chunk(chunk(turn_a(), 0, 0, 30, false), t).is_empty());
        let out = j.chunk(chunk(turn_a(), 1, 0, 30, false), t + ms(5));
        assert_eq!(order(&out), [1, 2]);
        assert_eq!(played(&out)[0].duration(), ms(30));
    }

    #[test]
    fn a_sentence_end_starts_playback_even_below_the_buffer() {
        let (mut j, t) = jitter();
        let out = j.chunk(chunk(turn_a(), 0, 0, 20, true), t);
        assert_eq!(order(&out), [1]);
    }

    #[test]
    fn an_empty_chunk_with_end_only_ends_the_sentence() {
        let (mut j, t) = jitter();
        assert!(j.chunk(chunk(turn_a(), 0, 0, 20, false), t).is_empty());
        let out = j.chunk(chunk(turn_a(), 1, 0, 0, true), t + ms(1));
        assert_eq!(order(&out), [1], "the 20 ms before the end are released");
        // Nothing to play, and no sound is invented.
        assert!(
            j.chunk(chunk(turn_a(), 2, 1, 0, true), t + ms(2))
                .is_empty()
        );
    }

    #[test]
    fn audio_that_waits_too_long_starts_anyway() {
        let (mut j, t) = jitter();
        assert!(j.chunk(chunk(turn_a(), 0, 0, 20, false), t).is_empty());
        assert!(j.poll(t + ms(150)).is_empty());
        assert_eq!(order(&j.poll(t + ms(200))), [1]);
    }

    #[test]
    fn orders_chunks_by_sequence_number() {
        let (mut j, t) = jitter();
        assert!(j.chunk(chunk(turn_a(), 2, 0, 40, false), t).is_empty());
        assert!(j.chunk(chunk(turn_a(), 1, 0, 40, false), t).is_empty());
        let out = j.chunk(chunk(turn_a(), 0, 0, 40, false), t + ms(10));
        assert_eq!(order(&out), [1, 2, 3]);
    }

    #[test]
    fn skips_a_missing_chunk_after_the_gap_wait() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        // Chunk 1 never comes.
        assert!(
            j.chunk(chunk(turn_a(), 2, 0, 100, false), t + ms(10))
                .is_empty()
        );
        assert!(j.poll(t + ms(100)).is_empty(), "still within the wait");
        let out = j.poll(t + ms(110));
        assert_eq!(order(&out), [3], "chunk 2 plays, chunk 1 is skipped");
        // The numbering goes on after the gap.
        let out = j.chunk(chunk(turn_a(), 3, 0, 100, false), t + ms(120));
        assert_eq!(order(&out), [4]);
    }

    #[test]
    fn a_late_chunk_after_the_skip_is_dropped() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        j.chunk(chunk(turn_a(), 2, 0, 100, false), t + ms(10));
        j.poll(t + ms(120));
        assert!(
            j.chunk(chunk(turn_a(), 1, 0, 100, false), t + ms(130))
                .is_empty()
        );
        // Same for a repeated chunk.
        assert!(
            j.chunk(chunk(turn_a(), 0, 0, 100, false), t + ms(131))
                .is_empty()
        );
    }

    #[test]
    fn a_whole_answer_arriving_at_once_is_all_kept() {
        let (mut j, t) = jitter();
        let mut all = Vec::new();
        for seq in 0..200 {
            // 12 s of voice in 60 ms chunks, all within a few milliseconds.
            all.extend(j.chunk(chunk(turn_a(), seq, seq / 20, 60, seq % 20 == 19), t));
        }
        let total: Duration = played(&all).iter().map(|p| p.duration()).sum();
        assert_eq!(total, ms(12_000));
        assert_eq!(all.len(), 200);
    }

    #[test]
    fn rebuffers_after_an_underrun() {
        let (mut j, t) = jitter();
        let out = j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        assert_eq!(order(&out), [1]);
        // While the 100 ms play, the next chunk goes straight through, even if short.
        let out = j.chunk(chunk(turn_a(), 1, 0, 20, false), t + ms(50));
        assert_eq!(order(&out), [2]);
        // 120 ms of sound were released: at 300 ms the output has been silent for a while.
        assert!(
            j.chunk(chunk(turn_a(), 2, 1, 30, false), t + ms(300))
                .is_empty()
        );
        let out = j.chunk(chunk(turn_a(), 3, 1, 30, false), t + ms(310));
        assert_eq!(order(&out), [3, 4]);
    }

    #[test]
    fn the_first_sound_of_a_sentence_carries_when_its_first_chunk_was_received() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, true), t);
        // Sentence 1 starts with chunk 1, which arrives after chunk 2.
        assert!(
            j.chunk(chunk(turn_a(), 2, 1, 100, true), t + ms(1))
                .is_empty()
        );
        let out = j.chunk(chunk(turn_a(), 1, 1, 100, false), t + ms(2));
        let plays = played(&out);
        assert_eq!(order(&out), [2, 3]);
        let mark = plays[0].mark.expect("chunk 1 starts sentence 1");
        assert_eq!((mark.sentence, mark.received), (1, t + ms(1)));
        assert!(plays[1].mark.is_none());
    }

    #[test]
    fn only_the_first_sound_of_a_sentence_is_marked() {
        let (mut j, t) = jitter();
        let mut marks = Vec::new();
        for seq in 0..6 {
            let sentence = seq / 3;
            let out = j.chunk(
                chunk(turn_a(), seq, sentence, 60, false),
                t + ms(seq as u64),
            );
            marks.extend(
                played(&out)
                    .iter()
                    .filter_map(|p| p.mark)
                    .map(|m| m.sentence),
            );
        }
        assert_eq!(marks, [0, 1]);
    }

    #[test]
    fn the_first_chunk_of_the_turn_is_marked() {
        let (mut j, t) = jitter();
        let out = j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        let mark = played(&out)[0].mark.unwrap();
        assert_eq!((mark.turn, mark.sentence, mark.received), (turn_a(), 0, t));
    }

    #[test]
    fn a_press_stops_and_drops_what_is_buffered() {
        let (mut j, t) = jitter();
        assert!(j.chunk(chunk(turn_a(), 0, 0, 30, false), t).is_empty()); // held
        j.chunk(chunk(turn_a(), 2, 0, 30, false), t); // waiting for 1
        let out = j.press(turn_b(), t);
        assert!(matches!(out.as_slice(), [Output::Flush(Cause::Button)]));
        // Nothing of what was held comes out later.
        assert!(j.poll(t + ms(500)).is_empty());
    }

    #[test]
    fn after_a_press_the_rest_of_the_old_turn_is_dropped() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        j.press(turn_b(), t);
        // The server may send up to ~100 ms more of turn A.
        for seq in 1..4 {
            assert!(
                j.chunk(chunk(turn_a(), seq, 0, 60, true), t + ms(20))
                    .is_empty()
            );
        }
        assert!(j.poll(t + ms(400)).is_empty());
    }

    #[test]
    fn after_a_press_the_new_turn_speaks_from_its_first_chunk() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        j.press(turn_b(), t);
        let out = j.chunk(chunk(turn_b(), 0, 0, 100, false), t + ms(2000));
        assert_eq!(order(&out), [1]);
    }

    #[test]
    fn a_press_makes_playback_start_again_with_the_buffer() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 1000, false), t);
        j.press(turn_b(), t);
        // The output was flushed: a short chunk right after must wait, not rely on `play_until`.
        assert!(
            j.chunk(chunk(turn_b(), 0, 0, 20, false), t + ms(10))
                .is_empty()
        );
    }

    #[test]
    fn the_newest_turn_wins_without_a_press() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_b(), 0, 0, 100, false), t);
        assert!(
            j.chunk(chunk(turn_a(), 0, 0, 100, false), t + ms(1))
                .is_empty()
        );
        let out = j.chunk(chunk(turn_b(), 1, 0, 100, false), t + ms(2));
        assert_eq!(order(&out), [2]);
    }

    #[test]
    fn a_newer_turn_restarts_the_numbering() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        j.chunk(chunk(turn_a(), 1, 0, 100, true), t);
        let out = j.chunk(chunk(turn_b(), 0, 0, 100, false), t + ms(5));
        assert_eq!(order(&out), [1]);
    }

    #[test]
    fn a_chunk_of_a_newer_turn_stops_the_old_one_like_a_press() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, false), t); // playing
        // Audio of the old turn waiting for the start buffer would play after the new turn.
        j.chunk(chunk(turn_a(), 1, 0, 20, false), t + ms(500));
        let out = j.chunk(chunk(turn_b(), 0, 0, 100, false), t + ms(510));
        assert!(
            matches!(out.first(), Some(Output::Flush(Cause::NewerTurn))),
            "{out:?}"
        );
        assert_eq!(out.len(), 2);
        assert_eq!(order(&out), [1], "only the new turn is played");
        // The rest of the old turn is dropped from now on.
        assert!(
            j.chunk(chunk(turn_a(), 2, 0, 100, false), t + ms(520))
                .is_empty()
        );
    }

    #[test]
    fn the_first_turn_and_the_turn_after_a_press_do_not_flush() {
        let (mut j, t) = jitter();
        let out = j.chunk(chunk(turn_a(), 0, 0, 100, false), t);
        assert!(matches!(out.as_slice(), [Output::Play(_)]));
        j.press(turn_b(), t);
        let out = j.chunk(chunk(turn_b(), 0, 0, 100, false), t + ms(5));
        assert!(matches!(out.as_slice(), [Output::Play(_)]));
    }

    #[test]
    fn an_older_turn_after_ten_seconds_of_silence_is_taken_as_a_new_turn() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_b(), 0, 0, 100, true), t);
        // Within the limit: still dropped.
        assert!(
            j.chunk(chunk(turn_a(), 0, 0, 100, false), t + ms(9_999))
                .is_empty()
        );
        // Past it: the clock went back, this is the new turn.
        let out = j.chunk(chunk(turn_a(), 0, 0, 100, false), t + ms(10_000));
        assert!(matches!(out.first(), Some(Output::Flush(Cause::NewerTurn))));
        assert_eq!(order(&out), [1]);
        // It is now the newest: its next chunks follow.
        assert_eq!(
            order(&j.chunk(chunk(turn_a(), 1, 0, 100, false), t + ms(10_010))),
            [2]
        );
    }

    #[test]
    fn dropped_chunks_do_not_count_as_activity_of_the_newest_turn() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_b(), 0, 0, 100, true), t);
        for n in 1..=3 {
            // An old turn that keeps sending every 4 s is dropped for 8 s, then taken over.
            let out = j.chunk(chunk(turn_a(), n, 0, 100, false), t + ms(4_000 * n as u64));
            assert_eq!(out.is_empty(), n < 3, "n = {n}");
        }
    }

    #[test]
    fn a_press_counts_as_activity_of_the_newest_turn() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_a(), 0, 0, 100, true), t);
        j.press(turn_b(), t + ms(9_000));
        // 12 s after the first chunk, but only 3 s after the press: still the old turn.
        assert!(
            j.chunk(chunk(turn_a(), 1, 0, 100, false), t + ms(12_000))
                .is_empty()
        );
    }

    #[test]
    fn the_numbering_wraps_instead_of_overflowing() {
        let (mut j, t) = jitter();
        // Walk to the end of the numbering the quick way: a gap skip to the last numbers.
        j.chunk(chunk(turn_a(), 0, 0, 20, false), t);
        j.chunk(chunk(turn_a(), u32::MAX - 1, 0, 20, false), t);
        j.poll(t + ms(150));
        let out = j.chunk(chunk(turn_a(), u32::MAX, 0, 20, false), t + ms(160));
        assert_eq!(played(&out).len(), 3);
        // The numbering starts again at 0.
        let out = j.chunk(chunk(turn_a(), 0, 0, 20, false), t + ms(170));
        assert_eq!(played(&out).len(), 1);
    }

    #[test]
    fn a_press_older_than_the_newest_turn_still_stops_but_does_not_lower_it() {
        let (mut j, t) = jitter();
        j.chunk(chunk(turn_b(), 0, 0, 100, false), t);
        let out = j.press(turn_a(), t);
        assert!(matches!(out.as_slice(), [Output::Flush(Cause::Button)]));
        assert!(
            j.chunk(chunk(turn_a(), 0, 0, 100, false), t + ms(1))
                .is_empty()
        );
    }
}
