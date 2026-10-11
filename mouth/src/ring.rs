//! The samples waiting for the output: a lock-free single-producer single-consumer ring, so the
//! audio callback never waits for the bus side and never allocates.
//!
//! The bus side owns the [`Writer`], the audio callback the [`Reader`]. Three things cross
//! between them, all without a lock:
//! - the samples (a `ringbuf` ring, sized for a whole answer);
//! - the marks of the first sample of each sentence (a second, small ring), which the reader
//!   turns into [`Played`] entries (a third ring) for the bus side to log;
//! - the flush: the writer publishes the absolute position of the end of what is to be thrown
//!   away (`discard_until`), the reader skips up to it before it plays anything. Positions count
//!   samples since the start, so a flush never eats the sound pushed after it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

use crate::jitter::Mark;

/// A sentence whose first sample has just gone to the output, and when.
pub type Played = (Mark, Instant);

/// The marks in flight: sentences, not samples, so a small number is plenty.
const MARKS: usize = 1024;

#[derive(Default)]
struct Shared {
    /// Everything before this position is to be thrown away.
    discard_until: AtomicU64,
    /// How many samples the reader has taken or skipped.
    consumed: AtomicU64,
}

pub struct Writer {
    samples: HeapProd<f32>,
    marks: HeapProd<(u64, Mark)>,
    pushed: u64,
    discarded: u64,
    shared: Arc<Shared>,
}

pub struct Reader {
    samples: HeapCons<f32>,
    marks: HeapCons<(u64, Mark)>,
    played: HeapProd<Played>,
    taken: u64,
    shared: Arc<Shared>,
}

pub struct PlayedReader(HeapCons<Played>);

/// A ring for `capacity` samples (all the channels counted).
pub fn channel(capacity: usize) -> (Writer, Reader, PlayedReader) {
    let shared = Arc::new(Shared::default());
    let (samples_in, samples_out) = HeapRb::<f32>::new(capacity).split();
    let (marks_in, marks_out) = HeapRb::<(u64, Mark)>::new(MARKS).split();
    let (played_in, played_out) = HeapRb::<Played>::new(MARKS).split();
    (
        Writer {
            samples: samples_in,
            marks: marks_in,
            pushed: 0,
            discarded: 0,
            shared: Arc::clone(&shared),
        },
        Reader {
            samples: samples_out,
            marks: marks_out,
            played: played_in,
            taken: 0,
            shared,
        },
        PlayedReader(played_out),
    )
}

impl Writer {
    /// Adds interleaved samples; `mark` is on the first one. All or nothing: returns how many
    /// samples were dropped because the ring is full (a whole answer is 60 s, so never).
    pub fn push(&mut self, samples: &[f32], mark: Option<Mark>) -> usize {
        if samples.is_empty() {
            return 0;
        }
        if samples.len() > self.samples.vacant_len() {
            return samples.len();
        }
        // The mark goes first: the reader that sees the sound sees its mark.
        if let Some(mark) = mark
            && self.marks.try_push((self.pushed, mark)).is_err()
        {
            tracing::warn!("too many sentences waiting: a mark is lost");
        }
        let written = self.samples.push_slice(samples);
        debug_assert_eq!(written, samples.len());
        self.pushed += written as u64;
        0
    }

    /// Throws away everything waiting, at once as far as the reader is concerned; returns how
    /// many samples were still to be played.
    pub fn flush(&mut self) -> u64 {
        let dropped = self.pending();
        self.discarded = self.pushed;
        self.shared
            .discard_until
            .store(self.pushed, Ordering::Release);
        dropped
    }

    /// How many samples are waiting to be played.
    pub fn pending(&self) -> u64 {
        let consumed = self.shared.consumed.load(Ordering::Acquire);
        self.pushed.saturating_sub(consumed.max(self.discarded))
    }
}

impl Reader {
    /// Fills `out` with the next samples and with silence beyond; returns how many were real.
    /// Does not allocate and does not wait.
    pub fn pop(&mut self, out: &mut [f32]) -> usize {
        let discard = self.shared.discard_until.load(Ordering::Acquire);
        if self.taken < discard {
            let skipped = self.samples.skip((discard - self.taken) as usize);
            self.taken += skipped as u64;
        }
        let before = self.taken;
        let n = if self.taken >= discard {
            self.samples.pop_slice(out)
        } else {
            0
        };
        out[n..].fill(0.0);
        self.taken += n as u64;
        self.shared.consumed.store(self.taken, Ordering::Release);

        let mut now = None;
        while let Some(&(at, mark)) = self.marks.try_peek() {
            if at < before {
                // Its sound was thrown away.
            } else if at < self.taken {
                let now = *now.get_or_insert_with(Instant::now);
                // The bus side polls often; if it does not, the oldest news is lost.
                let _ = self.played.try_push((mark, now));
            } else {
                break;
            }
            let _ = self.marks.try_pop();
        }
        n
    }
}

impl PlayedReader {
    pub fn drain(&mut self, out: &mut Vec<Played>) {
        while let Some(played) = self.0.try_pop() {
            out.push(played);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(sentence: u32) -> Mark {
        Mark {
            turn: contract::TurnId::new(),
            sentence,
            received: Instant::now(),
        }
    }

    fn played(rx: &mut PlayedReader) -> Vec<u32> {
        let mut out = Vec::new();
        rx.drain(&mut out);
        out.iter().map(|(m, _)| m.sentence).collect()
    }

    #[test]
    fn pops_in_order_and_pads_with_silence() {
        let (mut w, mut r, _) = channel(16);
        assert_eq!(w.push(&[1.0, 2.0, 3.0], None), 0);
        let mut out = [9.0; 5];
        assert_eq!(r.pop(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0, 0.0, 0.0]);
        assert_eq!(w.pending(), 0);
    }

    #[test]
    fn a_mark_is_reported_when_its_first_sample_is_taken() {
        let (mut w, mut r, mut rx) = channel(64);
        w.push(&[0.1; 4], Some(mark(0)));
        w.push(&[0.2; 4], Some(mark(1)));
        let mut out = [0.0; 2];
        r.pop(&mut out);
        assert_eq!(played(&mut rx), [0]);
        r.pop(&mut out); // still inside sentence 0
        assert!(played(&mut rx).is_empty());
        r.pop(&mut out);
        assert_eq!(played(&mut rx), [1]);
    }

    #[test]
    fn a_flush_drops_the_waiting_samples_and_marks_and_keeps_the_next_ones() {
        let (mut w, mut r, mut rx) = channel(256);
        w.push(&[0.5; 100], Some(mark(0)));
        let mut out = [0.0; 10];
        r.pop(&mut out);
        played(&mut rx);
        assert_eq!(w.flush(), 90);
        assert_eq!(w.pending(), 0);
        // New sound pushed before the reader even noticed the flush.
        w.push(&[0.9; 4], Some(mark(1)));
        let mut out = [0.0; 8];
        assert_eq!(r.pop(&mut out), 4);
        assert_eq!(out, [0.9, 0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(played(&mut rx), [1]);
    }

    #[test]
    fn a_flush_of_a_mark_never_reported_stays_silent() {
        let (mut w, mut r, mut rx) = channel(64);
        w.push(&[0.5; 10], Some(mark(7)));
        w.flush();
        let mut out = [0.0; 4];
        assert_eq!(r.pop(&mut out), 0);
        assert!(played(&mut rx).is_empty());
        w.push(&[0.5; 2], None);
        assert_eq!(r.pop(&mut out), 2);
        assert!(played(&mut rx).is_empty());
    }

    #[test]
    fn a_full_ring_drops_a_whole_chunk_and_does_not_misplace_marks() {
        let (mut w, mut r, mut rx) = channel(8);
        assert_eq!(w.push(&[1.0; 6], Some(mark(0))), 0);
        assert_eq!(w.push(&[2.0; 6], Some(mark(1))), 6);
        let mut out = [0.0; 6];
        r.pop(&mut out);
        assert_eq!(played(&mut rx), [0]);
        assert_eq!(w.push(&[3.0; 2], Some(mark(2))), 0);
        r.pop(&mut out);
        assert_eq!(played(&mut rx), [2]);
    }

    #[test]
    fn a_whole_answer_fits() {
        // 60 s at 48 kHz stereo.
        let (mut w, _r, _) = channel(60 * 48_000 * 2);
        let chunk = vec![0.0f32; 48_000 * 2];
        for _ in 0..60 {
            assert_eq!(w.push(&chunk, None), 0);
        }
        assert_eq!(w.pending(), 60 * 48_000 * 2);
    }

    #[test]
    fn works_across_threads_without_losing_or_reordering() {
        let (mut w, mut r, _) = channel(4096);
        let total = 2_000_000u32;
        let producer = std::thread::spawn(move || {
            let mut next = 0u32;
            while next < total {
                let n = 100.min(total - next);
                let chunk: Vec<f32> = (next..next + n).map(|v| v as f32).collect();
                if w.push(&chunk, None) == 0 {
                    next += n;
                } else {
                    std::thread::yield_now();
                }
            }
        });
        let mut expected = 0u32;
        let mut out = [0.0f32; 256];
        while expected < total {
            let n = r.pop(&mut out);
            for sample in &out[..n] {
                assert_eq!(*sample, expected as f32);
                expected += 1;
            }
            if n == 0 {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
    }
}
