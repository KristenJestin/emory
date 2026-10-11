//! La file d'un poste : les phrases du tour en cours, dites dans l'ordre de leur `numero`, et
//! l'état `Parle`. Une machine à états sans réseau ni horloge : les instants viennent de
//! l'appelant, les effets à exécuter sont rendus.

use std::{collections::BTreeMap, time::Duration, time::Instant};

use contract::{Dire, Etat, IdTour};

/// Ce que l'appelant doit faire.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Lancer la synthèse de cette phrase ; `seq` est le numéro du premier morceau.
    Synthesize {
        tour: IdTour,
        numero: u32,
        text: String,
        received: Instant,
        seq: u32,
    },
    /// Couper la synthèse en cours, tout de suite.
    Cut,
    /// Publier ce `Parle`.
    Parle(Etat),
}

pub struct Queue {
    grace: Duration,
    /// Le tour en cours, ou le dernier tour vu s'il est arrêté.
    tour: Option<IdTour>,
    stopped: bool,
    /// Le numéro de la prochaine phrase à dire.
    next: u32,
    waiting: BTreeMap<u32, (String, Instant)>,
    running: Option<u32>,
    /// Le numéro du prochain morceau de son du tour.
    seq: u32,
    speaking: bool,
    /// Quand le son envoyé jusqu'ici finit d'être joué, si le poste le joue sans trou.
    audio_end: Option<Instant>,
}

impl Queue {
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            tour: None,
            stopped: false,
            next: 0,
            waiting: BTreeMap::new(),
            running: None,
            seq: 0,
            speaking: false,
            audio_end: None,
        }
    }

    /// Une phrase à dire. Un tour plus récent remplace l'ancien ; une phrase d'un tour plus
    /// ancien, d'un tour arrêté, ou déjà vue, est ignorée.
    pub fn dire(&mut self, dire: &Dire, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        match self.tour {
            Some(t) if dire.tour < t => return effects,
            Some(t) if dire.tour == t => {
                if self.stopped
                    || dire.numero < self.next
                    || self.running == Some(dire.numero)
                    || self.waiting.contains_key(&dire.numero)
                {
                    return effects;
                }
            }
            _ => self.start_turn(dire.tour, &mut effects),
        }
        self.waiting.insert(dire.numero, (dire.texte.clone(), now));
        self.advance(&mut effects);
        effects
    }

    /// Le tour est arrêté : ce qui n'est pas dit est jeté, la synthèse coupée.
    pub fn stop(&mut self, tour: IdTour) -> Vec<Effect> {
        let mut effects = Vec::new();
        match self.tour {
            Some(t) if tour < t => return effects,
            Some(t) if tour == t => {
                if self.stopped {
                    return effects;
                }
            }
            _ => self.start_turn(tour, &mut effects),
        }
        self.stopped = true;
        self.silence(&mut effects);
        effects
    }

    /// La bouche s'arrête (le programme s'éteint) : plus rien n'est dit, `Parle` `fin` si besoin.
    pub fn shutdown(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.stopped = true;
        self.silence(&mut effects);
        effects
    }

    /// Du son de ce tour vient d'être envoyé : le premier du tour fait `Parle` `debut`.
    pub fn sound_sent(&mut self, tour: IdTour, duration: Duration, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !self.is_live(tour) || duration.is_zero() {
            return effects;
        }
        let from = self.audio_end.map_or(now, |end| end.max(now));
        self.audio_end = Some(from + duration);
        if !self.speaking {
            self.speaking = true;
            effects.push(Effect::Parle(Etat::Debut));
        }
        effects
    }

    /// La phrase `numero` est finie d'être envoyée ; `seq` est le prochain numéro de morceau.
    pub fn finished(&mut self, tour: IdTour, numero: u32, seq: u32) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.is_live(tour) && self.running == Some(numero) {
            self.running = None;
            self.next = numero + 1;
            self.seq = seq;
            self.advance(&mut effects);
        }
        effects
    }

    /// Quand appeler [`Queue::expire`] : la bouche parle, plus rien n'est à dire, et le poste a
    /// fini de jouer le son envoyé (plus un délai de grâce pour la phrase suivante de l'esprit).
    pub fn deadline(&self) -> Option<Instant> {
        if !self.speaking || self.running.is_some() || self.waiting.contains_key(&self.next) {
            return None;
        }
        self.audio_end.map(|end| end + self.grace)
    }

    /// `Parle` `fin` si l'échéance est passée.
    pub fn expire(&mut self, now: Instant) -> Vec<Effect> {
        match self.deadline() {
            Some(deadline) if now >= deadline => {
                self.speaking = false;
                self.audio_end = None;
                vec![Effect::Parle(Etat::Fin)]
            }
            _ => Vec::new(),
        }
    }

    fn is_live(&self, tour: IdTour) -> bool {
        self.tour == Some(tour) && !self.stopped
    }

    /// Un nouveau tour prend la place de l'ancien, qui est arrêté de la même façon.
    fn start_turn(&mut self, tour: IdTour, effects: &mut Vec<Effect>) {
        self.silence(effects);
        self.tour = Some(tour);
        self.stopped = false;
        self.next = 0;
        self.seq = 0;
    }

    fn silence(&mut self, effects: &mut Vec<Effect>) {
        self.waiting.clear();
        if self.running.take().is_some() {
            effects.push(Effect::Cut);
        }
        self.audio_end = None;
        if self.speaking {
            self.speaking = false;
            effects.push(Effect::Parle(Etat::Fin));
        }
    }

    fn advance(&mut self, effects: &mut Vec<Effect>) {
        if self.running.is_none()
            && let Some((text, received)) = self.waiting.remove(&self.next)
        {
            self.running = Some(self.next);
            effects.push(Effect::Synthesize {
                tour: self.tour.expect("a sentence waits only inside a turn"),
                numero: self.next,
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
    use contract::Appareil;

    const GRACE: Duration = Duration::from_millis(500);

    fn dire(tour: IdTour, numero: u32, texte: &str) -> Dire {
        Dire {
            tour,
            poste: Appareil::new("bureau").unwrap(),
            numero,
            texte: texte.to_owned(),
        }
    }

    fn said(effects: &[Effect]) -> Vec<(u32, &str, u32)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Synthesize {
                    numero, text, seq, ..
                } => Some((*numero, text.as_str(), *seq)),
                _ => None,
            })
            .collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn sentences_are_said_one_at_a_time_in_order() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        let fx = q.dire(&dire(tour, 0, "un"), t0);
        assert_eq!(said(&fx), [(0, "un", 0)]);
        // La suivante attend que la première soit finie.
        assert!(q.dire(&dire(tour, 1, "deux"), t0).is_empty());
        let fx = q.finished(tour, 0, 7);
        assert_eq!(
            said(&fx),
            [(1, "deux", 7)],
            "seq continues over the whole turn"
        );
    }

    #[test]
    fn an_early_sentence_waits_for_the_ones_before_it() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        assert!(q.dire(&dire(tour, 2, "trois"), t0).is_empty());
        assert!(q.dire(&dire(tour, 1, "deux"), t0).is_empty());
        assert_eq!(said(&q.dire(&dire(tour, 0, "un"), t0)), [(0, "un", 0)]);
        assert_eq!(said(&q.finished(tour, 0, 3)), [(1, "deux", 3)]);
        assert_eq!(said(&q.finished(tour, 1, 5)), [(2, "trois", 5)]);
    }

    #[test]
    fn duplicates_and_said_sentences_are_ignored() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        q.dire(&dire(tour, 0, "un"), t0);
        assert!(q.dire(&dire(tour, 0, "un"), t0).is_empty(), "running");
        q.dire(&dire(tour, 1, "deux"), t0);
        assert!(q.dire(&dire(tour, 1, "deux"), t0).is_empty(), "waiting");
        q.finished(tour, 0, 1);
        assert!(q.dire(&dire(tour, 0, "un"), t0).is_empty(), "already said");
    }

    #[test]
    fn stop_drops_what_waits_and_cuts_what_runs() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        q.dire(&dire(tour, 0, "un"), t0);
        q.dire(&dire(tour, 1, "deux"), t0);
        q.sound_sent(tour, ms(60), t0);
        let fx = q.stop(tour);
        assert_eq!(fx, [Effect::Cut, Effect::Parle(Etat::Fin)]);
        // Ni la fin de la phrase coupée ni une phrase en retard ne relancent rien.
        assert!(q.finished(tour, 0, 4).is_empty());
        assert!(q.dire(&dire(tour, 2, "trois"), t0).is_empty());
        assert!(q.stop(tour).is_empty(), "a second stop does nothing");
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn stop_with_nothing_running_does_not_cut() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        q.dire(&dire(tour, 0, "un"), t0);
        q.finished(tour, 0, 1);
        assert!(q.stop(tour).is_empty(), "silent, so no Parle either");
    }

    #[test]
    fn a_new_turn_replaces_the_old_one() {
        let (mut q, t0) = (Queue::new(GRACE), Instant::now());
        let (old, new) = (IdTour::nouveau(), IdTour::nouveau());
        q.dire(&dire(old, 0, "vieux un"), t0);
        q.dire(&dire(old, 1, "vieux deux"), t0);
        q.sound_sent(old, ms(60), t0);
        let fx = q.dire(&dire(new, 0, "neuf un"), t0);
        assert_eq!(
            fx[..2],
            [Effect::Cut, Effect::Parle(Etat::Fin)],
            "the old turn is stopped first"
        );
        assert_eq!(said(&fx), [(0, "neuf un", 0)], "numbering and seq restart");
        // Les événements de l'ancien tour n'ont plus d'effet.
        assert!(q.finished(old, 0, 9).is_empty());
        assert!(q.sound_sent(old, ms(60), t0).is_empty());
        assert!(q.dire(&dire(old, 2, "vieux trois"), t0).is_empty());
        assert_eq!(said(&q.finished(new, 0, 2)), []);
    }

    #[test]
    fn a_stop_for_an_unseen_turn_keeps_its_late_sentences_out() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        assert!(q.stop(tour).is_empty());
        assert!(q.dire(&dire(tour, 0, "en retard"), t0).is_empty());
    }

    #[test]
    fn parle_debut_once_then_fin_after_the_audio_has_played() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        q.dire(&dire(tour, 0, "un"), t0);
        assert_eq!(q.deadline(), None, "still synthesising");
        assert_eq!(
            q.sound_sent(tour, ms(100), t0),
            [Effect::Parle(Etat::Debut)]
        );
        assert!(
            q.sound_sent(tour, ms(100), t0 + ms(40)).is_empty(),
            "state unchanged"
        );
        q.finished(tour, 0, 2);
        // 200 ms de son envoyé à partir de t0 : joué jusqu'à t0+200, plus la grâce.
        assert_eq!(q.deadline(), Some(t0 + ms(200) + GRACE));
        assert!(
            q.expire(t0 + ms(600)).is_empty(),
            "the poste is still playing"
        );
        assert_eq!(q.expire(t0 + ms(700)), [Effect::Parle(Etat::Fin)]);
        assert!(q.expire(t0 + ms(800)).is_empty(), "fin is published once");
    }

    #[test]
    fn a_sentence_arriving_in_the_grace_keeps_the_speech_going() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        q.dire(&dire(tour, 0, "un"), t0);
        q.sound_sent(tour, ms(100), t0);
        q.finished(tour, 0, 2);
        let fx = q.dire(&dire(tour, 1, "deux"), t0 + ms(300));
        assert_eq!(said(&fx), [(1, "deux", 2)]);
        assert_eq!(q.deadline(), None);
        assert!(
            q.sound_sent(tour, ms(100), t0 + ms(400)).is_empty(),
            "no second debut"
        );
        q.finished(tour, 1, 4);
        assert_eq!(
            q.expire(t0 + ms(400) + ms(100) + GRACE),
            [Effect::Parle(Etat::Fin)]
        );
    }

    #[test]
    fn speech_after_a_fin_starts_again_with_debut() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        q.dire(&dire(tour, 0, "un"), t0);
        q.sound_sent(tour, ms(100), t0);
        q.finished(tour, 0, 2);
        q.expire(t0 + ms(2000));
        q.dire(&dire(tour, 1, "deux"), t0 + ms(3000));
        assert_eq!(
            q.sound_sent(tour, ms(100), t0 + ms(3100)),
            [Effect::Parle(Etat::Debut)]
        );
    }

    #[test]
    fn a_sentence_without_sound_does_not_make_the_bouche_speak() {
        let (mut q, t0, tour) = (Queue::new(GRACE), Instant::now(), IdTour::nouveau());
        q.dire(&dire(tour, 0, "."), t0);
        assert!(q.sound_sent(tour, Duration::ZERO, t0).is_empty());
        assert_eq!(q.finished(tour, 0, 1), []);
        assert_eq!(q.deadline(), None);
    }
}
