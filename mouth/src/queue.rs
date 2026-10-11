//! The samples waiting for the output. The bus side pushes, the audio thread of the output pops;
//! the lock is held for the copy of a few samples only.

use std::{collections::VecDeque, sync::Mutex};

use crate::jitter::Mark;

#[derive(Default)]
struct Inner {
    samples: VecDeque<f32>,
    /// The position (in samples since the start) where the sound of each mark begins.
    marks: VecDeque<(u64, Mark)>,
    pushed: u64,
    popped: u64,
}

#[derive(Default)]
pub struct Queue(Mutex<Inner>);

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic elsewhere must not mute the output for good.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds interleaved samples; `mark` is on the first one.
    pub fn push(&self, samples: Vec<f32>, mark: Option<Mark>) {
        let mut inner = self.lock();
        let at = inner.pushed;
        if let Some(mark) = mark {
            inner.marks.push_back((at, mark));
        }
        inner.pushed += samples.len() as u64;
        inner.samples.extend(samples);
    }

    /// Fills `out` from the queue and with silence beyond. Returns how many samples were real,
    /// and the marks whose first sample is among them.
    pub fn pop(&self, out: &mut [f32]) -> (usize, Vec<Mark>) {
        let mut inner = self.lock();
        let n = out.len().min(inner.samples.len());
        for (slot, sample) in out.iter_mut().zip(inner.samples.drain(..n)) {
            *slot = sample;
        }
        out[n..].fill(0.0);
        inner.popped += n as u64;
        let mut reached = Vec::new();
        while inner
            .marks
            .front()
            .is_some_and(|(at, _)| *at < inner.popped)
        {
            reached.extend(inner.marks.pop_front().map(|(_, mark)| mark));
        }
        (n, reached)
    }

    /// Drops everything waiting; returns how many samples were dropped.
    pub fn clear(&self) -> usize {
        let mut inner = self.lock();
        let dropped = inner.samples.len();
        inner.samples.clear();
        inner.marks.clear();
        inner.pushed = inner.popped;
        dropped
    }

    pub fn len(&self) -> usize {
        self.lock().samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn mark(sentence: u32) -> Mark {
        Mark {
            turn: contract::TurnId::new(),
            sentence,
            received: Instant::now(),
        }
    }

    #[test]
    fn pops_in_order_and_pads_with_silence() {
        let q = Queue::default();
        q.push(vec![1.0, 2.0, 3.0], None);
        let mut out = [9.0; 5];
        let (n, marks) = q.pop(&mut out);
        assert_eq!((n, out), (3, [1.0, 2.0, 3.0, 0.0, 0.0]));
        assert!(marks.is_empty() && q.is_empty());
    }

    #[test]
    fn a_mark_is_reported_when_its_first_sample_is_taken() {
        let q = Queue::default();
        q.push(vec![0.1; 4], Some(mark(0)));
        q.push(vec![0.2; 4], Some(mark(1)));
        let mut out = [0.0; 2];
        assert_eq!(q.pop(&mut out).1.len(), 1);
        assert!(q.pop(&mut out).1.is_empty()); // still inside sentence 0
        let (_, marks) = q.pop(&mut out);
        assert_eq!(marks.iter().map(|m| m.sentence).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn clear_drops_samples_and_marks_and_keeps_counting_right() {
        let q = Queue::default();
        q.push(vec![0.5; 100], Some(mark(0)));
        let mut out = [0.0; 10];
        q.pop(&mut out);
        assert_eq!(q.clear(), 90);
        assert!(q.is_empty());
        q.push(vec![0.5; 4], Some(mark(1)));
        let (n, marks) = q.pop(&mut out);
        assert_eq!((n, marks.len()), (4, 1));
    }
}
