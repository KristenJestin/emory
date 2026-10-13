//! The queue of a device: the sentences of the current turn, said in the order of their `index`,
//! and the `Speaking` state. A state machine without network or clock: the instants come from the
//! caller, the effects to run are returned.

use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

use contract::{Cue, CueKind, Say, State, TurnId};

use crate::cue::SENTENCE as CUE_SENTENCE;

/// What the caller must do.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Start the synthesis of this sentence; `seq` is the number of the first chunk.
    Synthesize {
        turn: TurnId,
        index: u32,
        text: String,
        received: Instant,
        seq: u32,
    },
    /// Play a cue sound, in one chunk numbered `seq` (it carries the end of its own sentence).
    PlayCue {
        turn: TurnId,
        sound: CueSound,
        seq: u32,
    },
    /// Cut the synthesis in progress, right now.
    Cut,
    /// Publish this `Speaking`.
    Speaking(State),
    /// The sentences `from..to` never arrived within the delay: they are skipped, the turn goes on
    /// with `to`.
    Skipped { turn: TurnId, from: u32, to: u32 },
}

/// A sound the voice draws itself (see [`crate::cue`]); a phrase to say goes through the synthesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueSound {
    Tone,
    Pulse,
}

/// The pulse stops by itself after this long: a mind that dies must not leave the voice beeping.
const PULSE_MAX: Duration = Duration::from_secs(30);

/// How many replaced turns the queue remembers.
const RETIRED_KEPT: usize = 16;

pub struct Queue {
    grace: Duration,
    /// How long the queue waits for a missing sentence before skipping it.
    gap_timeout: Duration,
    /// The current turn, or the last turn seen if it is stopped.
    turn: Option<TurnId>,
    /// The last replaced turns: their late sentences do not reopen the turn. The identifiers are
    /// not compared with each other, that would be trusting the clock of the ear.
    retired: VecDeque<TurnId>,
    stopped: bool,
    /// The number of the next sentence to say.
    next: u32,
    waiting: BTreeMap<u32, (String, Instant)>,
    /// Since when the sentence `next` is missing while later ones wait (and nothing runs).
    gap_since: Option<Instant>,
    running: Option<u32>,
    /// The index of the sentence marked `last` in the current turn, once it has been received.
    last: Option<u32>,
    /// The number of the next chunk of audio of the turn.
    seq: u32,
    speaking: bool,
    /// When the audio sent so far finishes playing, if the mouth plays it without a gap.
    audio_end: Option<Instant>,
    /// The first sentence of the answer was received: no cue starts after it.
    answer_started: bool,
    /// The phrases of the cue not started yet, in order.
    cue_phrases: VecDeque<(String, Instant)>,
    /// The sound of the phrase of the cue running has started to leave: it is no longer cut.
    cue_heard: bool,
    /// When the next pulse is due, while the pulse goes on, and when it gives up.
    pulse_next: Option<Instant>,
    pulse_until: Option<Instant>,
    pulse_interval: Duration,
}

impl Queue {
    pub fn new(grace: Duration, gap_timeout: Duration) -> Self {
        Self {
            grace,
            gap_timeout,
            turn: None,
            retired: VecDeque::new(),
            stopped: false,
            next: 0,
            waiting: BTreeMap::new(),
            gap_since: None,
            running: None,
            last: None,
            seq: 0,
            speaking: false,
            audio_end: None,
            answer_started: false,
            cue_phrases: VecDeque::new(),
            cue_heard: false,
            pulse_next: None,
            pulse_until: None,
            pulse_interval: Duration::from_millis(1600),
        }
    }

    /// The time between two pulses of the cue.
    pub fn with_pulse_interval(mut self, interval: Duration) -> Self {
        self.pulse_interval = interval;
        self
    }

    /// A waiting cue. It plays before the answer and never after its first sentence; a stopped
    /// or replaced turn takes none. A tone leaves at once; a pulse repeats until the answer
    /// starts; a phrase is synthesised like a sentence, before them.
    pub fn cue(&mut self, cue: &Cue, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.retired.contains(&cue.turn) {
            return effects;
        }
        match self.turn {
            Some(t) if cue.turn == t => {
                if self.stopped || self.answer_started {
                    return effects;
                }
            }
            _ => self.start_turn(cue.turn, &mut effects),
        }
        match cue.kind {
            CueKind::Tone => self.play(CueSound::Tone, &mut effects),
            CueKind::Pulse => {
                if self.pulse_next.is_none() {
                    self.play(CueSound::Pulse, &mut effects);
                    self.pulse_next = Some(now + self.pulse_interval);
                    self.pulse_until = Some(now + PULSE_MAX);
                }
            }
            CueKind::Speech => {
                let text = cue.text.as_deref().unwrap_or_default().trim();
                if !text.is_empty() {
                    self.cue_phrases.push_back((text.to_owned(), now));
                    self.advance(&mut effects);
                }
            }
        }
        effects
    }

    /// A tone or a pulse goes out, unless a sentence is being synthesised: the numbering of the
    /// chunks has one writer at a time.
    fn play(&mut self, sound: CueSound, effects: &mut Vec<Effect>) {
        if self.running.is_some() {
            return;
        }
        effects.push(Effect::PlayCue {
            turn: self.turn.expect("a cue plays only inside a turn"),
            sound,
            seq: self.seq,
        });
        self.seq = self.seq.saturating_add(1);
    }

    /// The answer starts: the cue is over. What has not started is dropped; a phrase whose sound
    /// has not left is cut (nothing was heard of it), one already leaving is said to its end and
    /// the answer follows it, so that no word is cut in the middle.
    fn end_cue(&mut self, effects: &mut Vec<Effect>) {
        self.pulse_next = None;
        self.pulse_until = None;
        self.cue_phrases.clear();
        if self.running == Some(CUE_SENTENCE) && !self.cue_heard {
            self.running = None;
            effects.push(Effect::Cut);
        }
    }

    /// A sentence to say. Another turn replaces the old one; a sentence of a replaced or stopped
    /// turn, or one already seen, is ignored.
    pub fn say(&mut self, say: &Say, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.retired.contains(&say.turn) {
            return effects;
        }
        match self.turn {
            Some(t) if say.turn == t => {
                if self.stopped
                    || say.index < self.next
                    || self.running == Some(say.index)
                    || self.waiting.contains_key(&say.index)
                {
                    return effects;
                }
            }
            _ => self.start_turn(say.turn, &mut effects),
        }
        if !self.answer_started {
            self.answer_started = true;
            self.end_cue(&mut effects);
        }
        if say.last {
            self.last = Some(say.index);
        }
        self.waiting.insert(say.index, (say.text.clone(), now));
        self.advance(&mut effects);
        self.watch_gap(now);
        effects
    }

    /// The turn is stopped: what is not said is thrown away, the synthesis cut. A `Stop` for a turn
    /// that is not the current one (a turn the queue never saw, or an old one) is only remembered
    /// as retired, so that its late sentences stay out: it never silences the current turn.
    pub fn stop(&mut self, turn: TurnId) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.turn != Some(turn) {
            self.retire(turn);
            return effects;
        }
        if self.stopped {
            return effects;
        }
        self.stopped = true;
        self.silence(&mut effects);
        effects
    }

    /// The voice stops (the program shuts down): nothing more is said, `Speaking` `end` if needed.
    pub fn shutdown(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.stopped = true;
        self.silence(&mut effects);
        effects
    }

    /// Audio of this turn has just been sent: the first of the turn makes `Speaking` `start`.
    pub fn sound_sent(&mut self, turn: TurnId, duration: Duration, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !self.is_live(turn) || duration.is_zero() {
            return effects;
        }
        if self.running == Some(CUE_SENTENCE) {
            self.cue_heard = true;
        }
        let from = self.audio_end.map_or(now, |end| end.max(now));
        self.audio_end = Some(from + duration);
        if !self.speaking {
            self.speaking = true;
            effects.push(Effect::Speaking(State::Start));
        }
        effects
    }

    /// The sentence `index` has finished being sent; `seq` is the next chunk number.
    pub fn finished(&mut self, turn: TurnId, index: u32, seq: u32, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.is_live(turn) && index == CUE_SENTENCE && self.running == Some(CUE_SENTENCE) {
            // The phrase of the cue: the sentences of the answer have not moved.
            self.running = None;
            self.seq = seq;
            self.advance(&mut effects);
            self.watch_gap(now);
        } else if self.is_live(turn) && self.running == Some(index) {
            self.running = None;
            self.next = index.saturating_add(1);
            self.seq = seq;
            self.advance(&mut effects);
            self.watch_gap(now);
        }
        effects
    }

    /// When to call [`Queue::expire`]: either the voice is speaking, nothing is left to say, and the
    /// mouth has finished playing the audio sent (once the sentence marked `last` has been said,
    /// that is the end; otherwise a grace delay is added for the next sentence of the mind); or a
    /// sentence is missing and the delay to wait for it ends.
    pub fn deadline(&self) -> Option<Instant> {
        let gap = self.gap_since.map(|since| since + self.gap_timeout);
        [gap, self.speaking_deadline(), self.pulse_next]
            .into_iter()
            .flatten()
            .min()
    }

    fn speaking_deadline(&self) -> Option<Instant> {
        if !self.speaking || self.running.is_some() || self.waiting.contains_key(&self.next) {
            return None;
        }
        let answer_complete = self.last.is_some_and(|last| self.next > last);
        let grace = if answer_complete {
            Duration::ZERO
        } else {
            self.grace
        };
        self.audio_end.map(|end| end + grace)
    }

    /// What is due at `now`: a missing sentence is skipped, and `Speaking` `end` is sent if its
    /// deadline has passed.
    pub fn expire(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self
            .gap_since
            .is_some_and(|since| now >= since + self.gap_timeout)
        {
            self.skip_missing(now, &mut effects);
        }
        if let Some(due) = self.pulse_next.filter(|due| now >= *due) {
            if self.pulse_until.is_some_and(|until| now >= until) {
                self.pulse_next = None;
                self.pulse_until = None;
            } else {
                self.play(CueSound::Pulse, &mut effects);
                self.pulse_next = Some(due.max(now) + self.pulse_interval);
            }
        }
        if self.speaking_deadline().is_some_and(|d| now >= d) {
            self.speaking = false;
            self.audio_end = None;
            effects.push(Effect::Speaking(State::End));
        }
        effects
    }

    /// Gives up on the sentences before the first one that waits.
    fn skip_missing(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        if let (Some(turn), Some(&to)) = (self.turn, self.waiting.keys().next()) {
            effects.push(Effect::Skipped {
                turn,
                from: self.next,
                to,
            });
            self.next = to;
            self.advance(effects);
        }
        self.watch_gap(now);
    }

    /// Starts or stops the wait for a missing sentence: later sentences wait, `next` is not there,
    /// and nothing runs that could be followed by it.
    fn watch_gap(&mut self, now: Instant) {
        let blocked = self.running.is_none()
            && !self.waiting.is_empty()
            && !self.waiting.contains_key(&self.next);
        if blocked {
            self.gap_since.get_or_insert(now);
        } else {
            self.gap_since = None;
        }
    }

    fn is_live(&self, turn: TurnId) -> bool {
        self.turn == Some(turn) && !self.stopped
    }

    /// A new turn takes the place of the old one, which is stopped the same way.
    fn start_turn(&mut self, turn: TurnId, effects: &mut Vec<Effect>) {
        self.silence(effects);
        if let Some(old) = self.turn.replace(turn) {
            self.retire(old);
        }
        self.stopped = false;
        self.next = 0;
        self.seq = 0;
        self.last = None;
        self.answer_started = false;
    }

    fn retire(&mut self, turn: TurnId) {
        if self.retired.contains(&turn) {
            return;
        }
        if self.retired.len() == RETIRED_KEPT {
            self.retired.pop_front();
        }
        self.retired.push_back(turn);
    }

    fn silence(&mut self, effects: &mut Vec<Effect>) {
        self.waiting.clear();
        self.cue_phrases.clear();
        self.pulse_next = None;
        self.pulse_until = None;
        self.gap_since = None;
        if self.running.take().is_some() {
            effects.push(Effect::Cut);
        }
        self.audio_end = None;
        if self.speaking {
            self.speaking = false;
            effects.push(Effect::Speaking(State::End));
        }
    }

    fn advance(&mut self, effects: &mut Vec<Effect>) {
        if self.running.is_none()
            && !self.answer_started
            && let Some((text, received)) = self.cue_phrases.pop_front()
        {
            self.running = Some(CUE_SENTENCE);
            self.cue_heard = false;
            effects.push(Effect::Synthesize {
                turn: self.turn.expect("a cue phrase waits only inside a turn"),
                index: CUE_SENTENCE,
                text,
                received,
                seq: self.seq,
            });
        } else if self.running.is_none()
            && let Some((text, received)) = self.waiting.remove(&self.next)
        {
            self.running = Some(self.next);
            effects.push(Effect::Synthesize {
                turn: self.turn.expect("a sentence waits only inside a turn"),
                index: self.next,
                text,
                received,
                seq: self.seq,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::Device;

    const GRACE: Duration = Duration::from_millis(500);
    const GAP: Duration = Duration::from_secs(3);

    fn say(turn: TurnId, index: u32, text: &str) -> Say {
        Say {
            turn,
            device: Device::new("office").unwrap(),
            index,
            text: text.to_owned(),
            last: false,
        }
    }

    fn say_last(turn: TurnId, index: u32, text: &str) -> Say {
        Say {
            last: true,
            ..say(turn, index, text)
        }
    }

    fn said(effects: &[Effect]) -> Vec<(u32, &str, u32)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Synthesize {
                    index, text, seq, ..
                } => Some((*index, text.as_str(), *seq)),
                _ => None,
            })
            .collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn sentences_are_said_one_at_a_time_in_order() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        let fx = q.say(&say(turn, 0, "un"), t0);
        assert_eq!(said(&fx), [(0, "un", 0)]);
        // The next one waits for the first to be finished.
        assert!(q.say(&say(turn, 1, "deux"), t0).is_empty());
        let fx = q.finished(turn, 0, 7, t0);
        assert_eq!(
            said(&fx),
            [(1, "deux", 7)],
            "seq continues over the whole turn"
        );
    }

    #[test]
    fn an_early_sentence_waits_for_the_ones_before_it() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        assert!(q.say(&say(turn, 2, "trois"), t0).is_empty());
        assert!(q.say(&say(turn, 1, "deux"), t0).is_empty());
        assert_eq!(said(&q.say(&say(turn, 0, "un"), t0)), [(0, "un", 0)]);
        assert_eq!(said(&q.finished(turn, 0, 3, t0)), [(1, "deux", 3)]);
        assert_eq!(said(&q.finished(turn, 1, 5, t0)), [(2, "trois", 5)]);
    }

    #[test]
    fn duplicates_and_said_sentences_are_ignored() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        assert!(q.say(&say(turn, 0, "un"), t0).is_empty(), "running");
        q.say(&say(turn, 1, "deux"), t0);
        assert!(q.say(&say(turn, 1, "deux"), t0).is_empty(), "waiting");
        q.finished(turn, 0, 1, t0);
        assert!(q.say(&say(turn, 0, "un"), t0).is_empty(), "already said");
    }

    #[test]
    fn stop_drops_what_waits_and_cuts_what_runs() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        q.say(&say(turn, 1, "deux"), t0);
        q.sound_sent(turn, ms(60), t0);
        let fx = q.stop(turn);
        assert_eq!(fx, [Effect::Cut, Effect::Speaking(State::End)]);
        // Neither the end of the cut sentence nor a late sentence restarts anything.
        assert!(q.finished(turn, 0, 4, t0).is_empty());
        assert!(q.say(&say(turn, 2, "trois"), t0).is_empty());
        assert!(q.stop(turn).is_empty(), "a second stop does nothing");
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn stop_with_nothing_running_does_not_cut() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        q.finished(turn, 0, 1, t0);
        assert!(q.stop(turn).is_empty(), "silent, so no Speaking either");
    }

    #[test]
    fn a_new_turn_replaces_the_old_one() {
        let (mut q, t0) = (Queue::new(GRACE, GAP), Instant::now());
        let (old, new) = (TurnId::new(), TurnId::new());
        q.say(&say(old, 0, "vieux un"), t0);
        q.say(&say(old, 1, "vieux deux"), t0);
        q.sound_sent(old, ms(60), t0);
        let fx = q.say(&say(new, 0, "neuf un"), t0);
        assert_eq!(
            fx[..2],
            [Effect::Cut, Effect::Speaking(State::End)],
            "the old turn is stopped first"
        );
        assert_eq!(said(&fx), [(0, "neuf un", 0)], "numbering and seq restart");
        // The events of the old turn have no effect any more.
        assert!(q.finished(old, 0, 9, t0).is_empty());
        assert!(q.sound_sent(old, ms(60), t0).is_empty());
        assert!(q.say(&say(old, 2, "vieux trois"), t0).is_empty());
        assert_eq!(said(&q.finished(new, 0, 2, t0)), []);
    }

    #[test]
    fn a_stop_for_an_unseen_turn_keeps_its_late_sentences_out() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        assert!(q.stop(turn).is_empty());
        assert!(q.say(&say(turn, 0, "en retard"), t0).is_empty());
    }

    #[test]
    fn speaking_start_once_then_end_after_the_audio_has_played() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        assert_eq!(q.deadline(), None, "still synthesising");
        assert_eq!(
            q.sound_sent(turn, ms(100), t0),
            [Effect::Speaking(State::Start)]
        );
        assert!(
            q.sound_sent(turn, ms(100), t0 + ms(40)).is_empty(),
            "state unchanged"
        );
        q.finished(turn, 0, 2, t0);
        // 200 ms of audio sent from t0: played until t0+200, plus the grace.
        assert_eq!(q.deadline(), Some(t0 + ms(200) + GRACE));
        assert!(
            q.expire(t0 + ms(600)).is_empty(),
            "the mouth is still playing"
        );
        assert_eq!(q.expire(t0 + ms(700)), [Effect::Speaking(State::End)]);
        assert!(q.expire(t0 + ms(800)).is_empty(), "end is published once");
    }

    #[test]
    fn the_end_comes_without_the_grace_once_the_last_sentence_has_been_played() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        assert_eq!(said(&q.say(&say_last(turn, 1, "deux"), t0)), []);
        q.sound_sent(turn, ms(100), t0);
        q.finished(turn, 0, 2, t0);
        // The last sentence is now running: nothing to expire yet.
        assert_eq!(q.deadline(), None);
        q.sound_sent(turn, ms(100), t0 + ms(40));
        q.finished(turn, 1, 4, t0);
        // 200 ms of audio sent from t0: the end is exactly when it has been played.
        assert_eq!(q.deadline(), Some(t0 + ms(200)));
        assert!(q.expire(t0 + ms(199)).is_empty());
        assert_eq!(q.expire(t0 + ms(200)), [Effect::Speaking(State::End)]);
        assert!(q.expire(t0 + ms(900)).is_empty(), "end is published once");
    }

    #[test]
    fn the_last_sentence_may_arrive_before_the_ones_it_follows() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        assert!(q.say(&say_last(turn, 1, "deux"), t0).is_empty());
        q.say(&say(turn, 0, "un"), t0);
        q.sound_sent(turn, ms(100), t0);
        q.finished(turn, 0, 2, t0);
        q.sound_sent(turn, ms(100), t0);
        q.finished(turn, 1, 4, t0);
        assert_eq!(q.deadline(), Some(t0 + ms(200)), "no grace");
    }

    #[test]
    fn without_a_last_sentence_the_grace_is_the_fallback() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        q.say(&say(turn, 1, "deux"), t0);
        q.sound_sent(turn, ms(100), t0);
        q.finished(turn, 0, 2, t0);
        q.sound_sent(turn, ms(100), t0);
        q.finished(turn, 1, 4, t0);
        // Nothing was marked `last`: the mind may still send one more sentence.
        assert_eq!(q.deadline(), Some(t0 + ms(200) + GRACE));
        assert!(q.expire(t0 + ms(200)).is_empty());
        assert_eq!(
            q.expire(t0 + ms(200) + GRACE),
            [Effect::Speaking(State::End)]
        );
    }

    #[test]
    fn the_last_mark_does_not_leak_into_the_next_turn() {
        let (mut q, t0) = (Queue::new(GRACE, GAP), Instant::now());
        let (first, second) = (TurnId::new(), TurnId::new());
        q.say(&say_last(first, 0, "un"), t0);
        q.finished(first, 0, 1, t0);
        q.say(&say(second, 0, "neuf"), t0);
        q.sound_sent(second, ms(100), t0);
        q.finished(second, 0, 2, t0);
        assert_eq!(q.deadline(), Some(t0 + ms(100) + GRACE));
    }

    #[test]
    fn a_sentence_arriving_in_the_grace_keeps_the_speech_going() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        q.sound_sent(turn, ms(100), t0);
        q.finished(turn, 0, 2, t0);
        let fx = q.say(&say(turn, 1, "deux"), t0 + ms(300));
        assert_eq!(said(&fx), [(1, "deux", 2)]);
        assert_eq!(q.deadline(), None);
        assert!(
            q.sound_sent(turn, ms(100), t0 + ms(400)).is_empty(),
            "no second start"
        );
        q.finished(turn, 1, 4, t0);
        assert_eq!(
            q.expire(t0 + ms(400) + ms(100) + GRACE),
            [Effect::Speaking(State::End)]
        );
    }

    #[test]
    fn speech_after_an_end_starts_again_with_start() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        q.sound_sent(turn, ms(100), t0);
        q.finished(turn, 0, 2, t0);
        q.expire(t0 + ms(2000));
        q.say(&say(turn, 1, "deux"), t0 + ms(3000));
        assert_eq!(
            q.sound_sent(turn, ms(100), t0 + ms(3100)),
            [Effect::Speaking(State::Start)]
        );
    }

    #[test]
    fn a_sentence_without_sound_does_not_make_the_voice_speak() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "."), t0);
        assert!(q.sound_sent(turn, Duration::ZERO, t0).is_empty());
        assert_eq!(q.finished(turn, 0, 1, t0), []);
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn a_new_turn_wins_even_if_its_id_sorts_before_the_old_one() {
        // The ear's clock may step back: the queue must not trust the order of the ids.
        let (mut q, t0) = (Queue::new(GRACE, GAP), Instant::now());
        let (a, b) = (TurnId::new(), TurnId::new());
        let (old, new) = (a.max(b), a.min(b));
        q.say(&say(old, 0, "vieux"), t0);
        assert_eq!(said(&q.say(&say(new, 0, "neuf"), t0)), [(0, "neuf", 0)]);
    }

    #[test]
    fn a_late_stop_for_an_unseen_turn_does_not_touch_the_current_turn() {
        let (mut q, t0) = (Queue::new(GRACE, GAP), Instant::now());
        let (current, unseen) = (TurnId::new(), TurnId::new());
        q.say(&say(current, 0, "un"), t0);
        q.say(&say(current, 1, "deux"), t0);
        q.sound_sent(current, ms(60), t0);
        // The stop of a turn the queue never saw: nothing is cut, nothing is retired but it.
        assert!(q.stop(unseen).is_empty());
        assert!(
            q.say(&say(unseen, 0, "en retard"), t0).is_empty(),
            "its late sentences stay out"
        );
        // The current turn goes on, as if nothing had happened.
        assert_eq!(said(&q.finished(current, 0, 4, t0)), [(1, "deux", 4)]);
        assert!(q.sound_sent(current, ms(60), t0).is_empty());
        // And it can still be stopped.
        assert_eq!(q.stop(current), [Effect::Cut, Effect::Speaking(State::End)]);
    }

    #[test]
    fn a_stop_for_an_old_turn_does_not_touch_the_new_one() {
        let (mut q, t0) = (Queue::new(GRACE, GAP), Instant::now());
        let (old, new) = (TurnId::new(), TurnId::new());
        q.say(&say(old, 0, "vieux"), t0);
        q.say(&say(new, 0, "neuf"), t0);
        assert!(q.stop(old).is_empty());
        assert_eq!(q.finished(new, 0, 2, t0), []);
        assert_eq!(q.stop(new), []);
    }

    #[test]
    fn a_missing_sentence_is_skipped_after_the_delay() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "un"), t0);
        q.say(&say(turn, 2, "trois"), t0 + ms(10));
        // Sentence 0 is being said: nobody waits for 1 yet.
        assert_eq!(q.deadline(), None);
        q.finished(turn, 0, 2, t0 + ms(100));
        // Now 1 is missing and 2 waits: the delay runs from here.
        assert_eq!(q.deadline(), Some(t0 + ms(100) + GAP));
        assert!(q.expire(t0 + ms(100) + GAP - ms(1)).is_empty());
        let fx = q.expire(t0 + ms(100) + GAP);
        assert_eq!(
            fx[0],
            Effect::Skipped {
                turn,
                from: 1,
                to: 2
            }
        );
        assert_eq!(said(&fx), [(2, "trois", 2)], "the turn goes on with 2");
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn several_missing_sentences_are_skipped_at_once() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 3, "quatre"), t0);
        assert_eq!(q.deadline(), Some(t0 + GAP));
        let fx = q.expire(t0 + GAP);
        assert_eq!(
            fx[0],
            Effect::Skipped {
                turn,
                from: 0,
                to: 3
            }
        );
        assert_eq!(said(&fx), [(3, "quatre", 0)]);
        // The next ones are in order again.
        assert!(q.say(&say(turn, 4, "cinq"), t0 + GAP).is_empty());
        assert_eq!(said(&q.finished(turn, 3, 5, t0 + GAP)), [(4, "cinq", 5)]);
    }

    #[test]
    fn the_wait_ends_when_the_missing_sentence_arrives() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, 1, "deux"), t0);
        assert_eq!(q.deadline(), Some(t0 + GAP));
        assert_eq!(
            said(&q.say(&say(turn, 0, "un"), t0 + ms(500))),
            [(0, "un", 0)]
        );
        assert_eq!(q.deadline(), None, "nothing is missing any more");
        assert!(q.expire(t0 + GAP).is_empty());
    }

    #[test]
    fn a_stop_or_a_new_turn_ends_the_wait_for_a_missing_sentence() {
        let (mut q, t0) = (Queue::new(GRACE, GAP), Instant::now());
        let (a, b) = (TurnId::new(), TurnId::new());
        q.say(&say(a, 2, "trois"), t0);
        q.stop(a);
        assert_eq!(q.deadline(), None);
        q.say(&say(b, 2, "trois"), t0);
        assert_eq!(q.deadline(), Some(t0 + GAP));
        q.say(&say(TurnId::new(), 0, "autre"), t0 + ms(1));
        assert_eq!(q.deadline(), None, "the new turn starts clean");
    }

    #[test]
    fn the_index_after_the_largest_one_does_not_overflow() {
        let (mut q, t0, turn) = (Queue::new(GRACE, GAP), Instant::now(), TurnId::new());
        q.say(&say(turn, u32::MAX, "dernier"), t0);
        let fx = q.expire(t0 + GAP);
        assert_eq!(said(&fx), [(u32::MAX, "dernier", 0)]);
        assert_eq!(q.finished(turn, u32::MAX, 1, t0 + GAP), []);
    }

    // The waiting cue.

    fn cue(turn: TurnId, kind: CueKind, text: Option<&str>) -> Cue {
        Cue {
            turn,
            device: Device::new("office").unwrap(),
            kind,
            text: text.map(str::to_owned),
        }
    }

    fn sounds(effects: &[Effect]) -> Vec<(CueSound, u32)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::PlayCue { sound, seq, .. } => Some((*sound, *seq)),
                _ => None,
            })
            .collect()
    }

    fn cue_queue() -> Queue {
        Queue::new(GRACE, GAP).with_pulse_interval(ms(1000))
    }

    #[test]
    fn a_tone_leaves_at_once_and_the_answer_numbers_on_from_it() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        let effects = q.cue(&cue(turn, CueKind::Tone, None), t0);
        assert_eq!(sounds(&effects), [(CueSound::Tone, 0)]);
        // No Speaking: a tone is not speech.
        assert!(!effects.iter().any(|e| matches!(e, Effect::Speaking(_))));
        let effects = q.say(&say(turn, 0, "Bonjour."), t0 + ms(900));
        assert_eq!(said(&effects), [(0, "Bonjour.", 1)]);
    }

    #[test]
    fn the_pulse_repeats_until_the_first_sentence_and_no_longer() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        q.cue(&cue(turn, CueKind::Tone, None), t0);
        let effects = q.cue(&cue(turn, CueKind::Pulse, None), t0 + ms(1500));
        assert_eq!(sounds(&effects), [(CueSound::Pulse, 1)]);
        assert_eq!(q.deadline(), Some(t0 + ms(2500)));
        assert!(q.expire(t0 + ms(2000)).is_empty());
        let effects = q.expire(t0 + ms(2500));
        assert_eq!(sounds(&effects), [(CueSound::Pulse, 2)]);
        // A second ask for the pulse does not double it.
        assert!(
            q.cue(&cue(turn, CueKind::Pulse, None), t0 + ms(2600))
                .is_empty()
        );
        assert_eq!(q.deadline(), Some(t0 + ms(3500)));
        // The first word: the pulse is over, the sentence goes out with the next number.
        let effects = q.say(&say(turn, 0, "Voilà."), t0 + ms(3000));
        assert_eq!(said(&effects), [(0, "Voilà.", 3)]);
        assert_eq!(q.deadline(), None);
        assert!(q.expire(t0 + ms(3600)).is_empty());
    }

    #[test]
    fn nothing_of_the_cue_starts_after_the_answer_has() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        q.say(&say(turn, 0, "Voilà."), t0);
        for kind in [CueKind::Tone, CueKind::Pulse] {
            assert!(q.cue(&cue(turn, kind, None), t0 + ms(10)).is_empty());
        }
        let late = cue(turn, CueKind::Speech, Some("Je regarde."));
        assert!(q.cue(&late, t0 + ms(10)).is_empty());
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn a_stopped_turn_takes_no_cue_and_a_stop_ends_the_pulse() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        q.cue(&cue(turn, CueKind::Pulse, None), t0);
        assert!(q.deadline().is_some());
        q.stop(turn);
        assert_eq!(q.deadline(), None);
        assert!(
            q.cue(&cue(turn, CueKind::Tone, None), t0 + ms(5))
                .is_empty()
        );
        // A stop heard before the turn was ever seen keeps its cues out as well.
        let other = TurnId::new();
        q.stop(other);
        assert!(
            q.cue(&cue(other, CueKind::Tone, None), t0 + ms(5))
                .is_empty()
        );
    }

    #[test]
    fn a_new_turn_ends_the_pulse_of_the_old_one() {
        let (mut q, t0, old, new) = (cue_queue(), Instant::now(), TurnId::new(), TurnId::new());
        q.cue(&cue(old, CueKind::Pulse, None), t0);
        let effects = q.cue(&cue(new, CueKind::Tone, None), t0 + ms(100));
        // The new turn numbers from 0, and the old pulse no longer comes due.
        assert_eq!(sounds(&effects), [(CueSound::Tone, 0)]);
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn the_pulse_gives_up_by_itself() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        q.cue(&cue(turn, CueKind::Pulse, None), t0);
        let mut now = t0;
        let mut count = 0;
        while let Some(due) = q.deadline() {
            now = due;
            count += sounds(&q.expire(now)).len();
            assert!(now < t0 + Duration::from_secs(40), "it never ends");
        }
        assert!(count >= 20 && now >= t0 + PULSE_MAX);
    }

    #[test]
    fn a_phrase_is_synthesised_and_the_answer_follows_it() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        let effects = q.cue(&cue(turn, CueKind::Speech, Some(" Je regarde. ")), t0);
        assert_eq!(said(&effects), [(CUE_SENTENCE, "Je regarde.", 0)]);
        // The sound of the phrase leaves: Speaking starts, and the phrase will not be cut.
        let effects = q.sound_sent(turn, ms(800), t0 + ms(200));
        assert_eq!(effects, [Effect::Speaking(State::Start)]);
        // The answer arrives while the phrase is on its way: it waits, nothing is cut.
        let effects = q.say(&say(turn, 0, "Voilà."), t0 + ms(400));
        assert!(effects.is_empty());
        // The phrase is sent (3 chunks): the answer starts, numbered on from it.
        let effects = q.finished(turn, CUE_SENTENCE, 3, t0 + ms(500));
        assert_eq!(said(&effects), [(0, "Voilà.", 3)]);
    }

    #[test]
    fn a_phrase_whose_sound_has_not_left_is_dropped_by_the_answer() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        q.cue(&cue(turn, CueKind::Speech, Some("Je regarde.")), t0);
        let effects = q.say(&say(turn, 0, "Voilà."), t0 + ms(100));
        // Nothing was heard of the phrase: it is cut and the answer takes its place at once.
        assert_eq!(effects[0], Effect::Cut);
        assert_eq!(said(&effects), [(0, "Voilà.", 0)]);
    }

    #[test]
    fn a_second_phrase_waits_for_the_first_and_is_dropped_when_the_answer_starts() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        q.cue(&cue(turn, CueKind::Speech, Some("Je regarde.")), t0);
        q.sound_sent(turn, ms(800), t0 + ms(100));
        let effects = q.cue(
            &cue(turn, CueKind::Speech, Some("Voyons voir.")),
            t0 + ms(200),
        );
        assert!(said(&effects).is_empty(), "one phrase at a time");
        let effects = q.finished(turn, CUE_SENTENCE, 2, t0 + ms(400));
        assert_eq!(said(&effects), [(CUE_SENTENCE, "Voyons voir.", 2)]);
        // The answer comes while the second one has not left: it is cut.
        let effects = q.say(&say(turn, 0, "Voilà."), t0 + ms(450));
        assert_eq!(effects[0], Effect::Cut);
        assert_eq!(said(&effects), [(0, "Voilà.", 2)]);
        // A phrase queued behind a running one never starts once the answer has.
        let (mut q, turn) = (cue_queue(), TurnId::new());
        q.cue(&cue(turn, CueKind::Speech, Some("Un.")), t0);
        q.sound_sent(turn, ms(500), t0 + ms(50));
        q.cue(&cue(turn, CueKind::Speech, Some("Deux.")), t0 + ms(60));
        q.say(&say(turn, 0, "Voilà."), t0 + ms(70));
        let effects = q.finished(turn, CUE_SENTENCE, 2, t0 + ms(300));
        assert_eq!(said(&effects), [(0, "Voilà.", 2)]);
    }

    #[test]
    fn a_blank_phrase_says_nothing() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        assert!(
            q.cue(&cue(turn, CueKind::Speech, Some("  ")), t0)
                .is_empty()
        );
        assert!(q.cue(&cue(turn, CueKind::Speech, None), t0).is_empty());
    }

    #[test]
    fn a_stop_cuts_the_phrase_and_a_sound_of_the_old_turn_is_not_taken_for_it() {
        let (mut q, t0, turn) = (cue_queue(), Instant::now(), TurnId::new());
        q.cue(&cue(turn, CueKind::Speech, Some("Je regarde.")), t0);
        let effects = q.stop(turn);
        assert_eq!(effects, [Effect::Cut]);
        assert!(q.finished(turn, CUE_SENTENCE, 2, t0 + ms(10)).is_empty());
    }
}
