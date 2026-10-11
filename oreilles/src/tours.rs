//! La machine d'états des tours : un tour ouvert au plus par poste, du `debut` à la `fin`.
//!
//! Elle ne connaît ni le bus ni les modèles : elle suit les messages (début, morceaux de son
//! numérotés, fin, temps qui passe) et dit quel tour s'ouvre, reçoit du son ou se ferme. Chaque tour
//! porte une `session` que l'appelant fabrique, et qu'elle lui rend à la fermeture.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use contract::{Appareil, IdTour, Quand};

/// Pourquoi un tour se ferme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Raison {
    /// Le second appui.
    Fin,
    /// Un nouveau `debut` sur un poste dont le tour était encore ouvert.
    Remplace,
    /// Plus de son depuis [`contract::son::SILENCE_QUI_FERME_LE_TOUR`] : le poste est mort.
    Silence,
    /// Le programme s'arrête.
    Arret,
}

/// Un trou dans la numérotation du son : `attendu..recu` n'est jamais arrivé.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trou {
    pub attendu: u32,
    pub recu: u32,
}

/// Un tour fermé, rendu avec sa session.
#[derive(Debug)]
pub struct Ferme<S> {
    pub poste: Appareil,
    pub tour: IdTour,
    pub debut: Quand,
    pub raison: Raison,
    pub session: S,
}

/// Ce qui arrive à un `debut`.
#[derive(Debug)]
pub enum Debut<S> {
    /// Le tour est ouvert ; `remplace` est celui qu'il a fermé sur ce poste, s'il y en avait un.
    Ouvert { remplace: Option<Ferme<S>> },
    /// Ce tour est déjà ouvert (message reçu deux fois) : rien ne change.
    Doublon,
}

/// Ce qui arrive à un morceau de son.
#[derive(Debug)]
pub enum Son<'a, S> {
    /// Pas de tour ouvert pour ce poste et cet identifiant : le son est jeté.
    Inconnu,
    /// Un morceau déjà vu, ou en retard sur ceux qui ont suivi : il est jeté.
    Ancien,
    /// Le morceau est pour la session ; `trou` dit ce qui a manqué avant lui.
    Accepte {
        session: &'a mut S,
        trou: Option<Trou>,
    },
}

struct Ouvert<S> {
    tour: IdTour,
    debut: Quand,
    prochain_seq: u32,
    dernier_son: Instant,
    session: S,
}

pub struct Postes<S> {
    ouverts: HashMap<Appareil, Ouvert<S>>,
    silence: Duration,
}

impl<S> Postes<S> {
    pub fn new(silence: Duration) -> Self {
        Self {
            ouverts: HashMap::new(),
            silence,
        }
    }

    pub fn nombre_ouverts(&self) -> usize {
        self.ouverts.len()
    }

    /// Le poste commence un tour. Le silence se compte à partir de maintenant.
    pub fn debut(
        &mut self,
        poste: &Appareil,
        tour: IdTour,
        quand: Quand,
        maintenant: Instant,
        creer: impl FnOnce() -> S,
    ) -> Debut<S> {
        if self.ouverts.get(poste).is_some_and(|o| o.tour == tour) {
            return Debut::Doublon;
        }
        let remplace = self.fermer(poste, Raison::Remplace);
        self.ouverts.insert(
            poste.clone(),
            Ouvert {
                tour,
                debut: quand,
                prochain_seq: 0,
                dernier_son: maintenant,
                session: creer(),
            },
        );
        Debut::Ouvert { remplace }
    }

    /// Un morceau de son numéroté `seq` pour le tour `tour` du poste.
    pub fn son(
        &mut self,
        poste: &Appareil,
        tour: IdTour,
        seq: u32,
        maintenant: Instant,
    ) -> Son<'_, S> {
        let Some(ouvert) = self.ouverts.get_mut(poste).filter(|o| o.tour == tour) else {
            return Son::Inconnu;
        };
        if seq < ouvert.prochain_seq {
            return Son::Ancien;
        }
        let trou = (seq > ouvert.prochain_seq).then_some(Trou {
            attendu: ouvert.prochain_seq,
            recu: seq,
        });
        ouvert.prochain_seq = seq.saturating_add(1);
        ouvert.dernier_son = maintenant;
        Son::Accepte {
            session: &mut ouvert.session,
            trou,
        }
    }

    /// Le second appui. `None` quand ce tour n'est pas (ou plus) ouvert : il a pu être fermé par
    /// le silence ou remplacé.
    pub fn fin(&mut self, poste: &Appareil, tour: IdTour) -> Option<Ferme<S>> {
        if self.ouverts.get(poste)?.tour != tour {
            return None;
        }
        self.fermer(poste, Raison::Fin)
    }

    /// Ferme les tours qui ne reçoivent plus de son depuis assez longtemps.
    pub fn expirer(&mut self, maintenant: Instant) -> Vec<Ferme<S>> {
        let perimes: Vec<Appareil> = self
            .ouverts
            .iter()
            .filter(|(_, o)| maintenant.saturating_duration_since(o.dernier_son) >= self.silence)
            .map(|(poste, _)| poste.clone())
            .collect();
        perimes
            .iter()
            .filter_map(|poste| self.fermer(poste, Raison::Silence))
            .collect()
    }

    /// Ferme tout, à l'arrêt du programme.
    pub fn tout_fermer(&mut self) -> Vec<Ferme<S>> {
        let postes: Vec<Appareil> = self.ouverts.keys().cloned().collect();
        postes
            .iter()
            .filter_map(|poste| self.fermer(poste, Raison::Arret))
            .collect()
    }

    fn fermer(&mut self, poste: &Appareil, raison: Raison) -> Option<Ferme<S>> {
        let ouvert = self.ouverts.remove(poste)?;
        Some(Ferme {
            poste: poste.clone(),
            tour: ouvert.tour,
            debut: ouvert.debut,
            raison,
            session: ouvert.session,
        })
    }
}

#[cfg(test)]
mod tests {
    use contract::son::SILENCE_QUI_FERME_LE_TOUR;

    use super::*;

    fn poste(nom: &str) -> Appareil {
        nom.parse().unwrap()
    }

    fn machine() -> Postes<Vec<u32>> {
        Postes::new(SILENCE_QUI_FERME_LE_TOUR)
    }

    fn ouvrir(m: &mut Postes<Vec<u32>>, p: &Appareil, t: IdTour, now: Instant) -> Debut<Vec<u32>> {
        m.debut(p, t, contract::maintenant(), now, Vec::new)
    }

    fn donner(m: &mut Postes<Vec<u32>>, p: &Appareil, t: IdTour, seq: u32, now: Instant) {
        match m.son(p, t, seq, now) {
            Son::Accepte { session, .. } => session.push(seq),
            autre => panic!("son refusé : {autre:?}"),
        }
    }

    #[test]
    fn un_tour_de_son_a_fin() {
        let (mut m, p, t, now) = (
            machine(),
            poste("bureau"),
            IdTour::nouveau(),
            Instant::now(),
        );
        assert!(matches!(
            ouvrir(&mut m, &p, t, now),
            Debut::Ouvert { remplace: None }
        ));
        for seq in 0..3 {
            donner(&mut m, &p, t, seq, now);
        }
        let ferme = m.fin(&p, t).unwrap();
        assert_eq!((ferme.raison, ferme.tour), (Raison::Fin, t));
        assert_eq!(ferme.session, vec![0, 1, 2]);
        assert_eq!(m.nombre_ouverts(), 0);
    }

    #[test]
    fn un_trou_est_signale_et_le_son_continue() {
        let (mut m, p, t, now) = (
            machine(),
            poste("bureau"),
            IdTour::nouveau(),
            Instant::now(),
        );
        ouvrir(&mut m, &p, t, now);
        donner(&mut m, &p, t, 0, now);
        match m.son(&p, t, 4, now) {
            Son::Accepte { trou, .. } => {
                assert_eq!(
                    trou,
                    Some(Trou {
                        attendu: 1,
                        recu: 4
                    })
                );
            }
            autre => panic!("{autre:?}"),
        }
        match m.son(&p, t, 5, now) {
            Son::Accepte { trou, .. } => assert_eq!(trou, None),
            autre => panic!("{autre:?}"),
        }
    }

    #[test]
    fn un_morceau_repete_ou_en_retard_est_jete() {
        let (mut m, p, t, now) = (
            machine(),
            poste("bureau"),
            IdTour::nouveau(),
            Instant::now(),
        );
        ouvrir(&mut m, &p, t, now);
        donner(&mut m, &p, t, 0, now);
        donner(&mut m, &p, t, 3, now);
        assert!(matches!(m.son(&p, t, 3, now), Son::Ancien));
        assert!(matches!(m.son(&p, t, 2, now), Son::Ancien));
        assert_eq!(m.fin(&p, t).unwrap().session, vec![0, 3]);
    }

    #[test]
    fn le_son_d_un_autre_tour_ou_sans_tour_est_inconnu() {
        let (mut m, p, t, now) = (
            machine(),
            poste("bureau"),
            IdTour::nouveau(),
            Instant::now(),
        );
        assert!(matches!(m.son(&p, t, 0, now), Son::Inconnu));
        ouvrir(&mut m, &p, t, now);
        assert!(matches!(m.son(&p, IdTour::nouveau(), 0, now), Son::Inconnu));
        assert!(matches!(m.son(&poste("salon"), t, 0, now), Son::Inconnu));
    }

    #[test]
    fn un_nouveau_debut_ferme_l_ancien_tour_du_poste() {
        let (mut m, p, now) = (machine(), poste("bureau"), Instant::now());
        let (premier, second) = (IdTour::nouveau(), IdTour::nouveau());
        ouvrir(&mut m, &p, premier, now);
        donner(&mut m, &p, premier, 0, now);
        let Debut::Ouvert { remplace } = ouvrir(&mut m, &p, second, now) else {
            panic!("le second tour devait s'ouvrir");
        };
        let ferme = remplace.expect("l'ancien tour est fermé");
        assert_eq!((ferme.raison, ferme.tour), (Raison::Remplace, premier));
        assert_eq!(ferme.session, vec![0]);
        // La numérotation repart à 0 pour le nouveau tour ; l'ancien n'écoute plus.
        assert!(matches!(m.son(&p, premier, 1, now), Son::Inconnu));
        donner(&mut m, &p, second, 0, now);
        // La fin de l'ancien tour, arrivée en retard, ne ferme pas le nouveau.
        assert!(m.fin(&p, premier).is_none());
        assert_eq!(m.nombre_ouverts(), 1);
    }

    #[test]
    fn un_debut_repete_ne_change_rien() {
        let (mut m, p, t, now) = (
            machine(),
            poste("bureau"),
            IdTour::nouveau(),
            Instant::now(),
        );
        ouvrir(&mut m, &p, t, now);
        donner(&mut m, &p, t, 0, now);
        assert!(matches!(ouvrir(&mut m, &p, t, now), Debut::Doublon));
        assert_eq!(m.fin(&p, t).unwrap().session, vec![0]);
    }

    #[test]
    fn deux_postes_ont_chacun_leur_tour() {
        let (mut m, now) = (machine(), Instant::now());
        let (bureau, salon) = (poste("bureau"), poste("salon"));
        let (t1, t2) = (IdTour::nouveau(), IdTour::nouveau());
        ouvrir(&mut m, &bureau, t1, now);
        assert!(matches!(
            ouvrir(&mut m, &salon, t2, now),
            Debut::Ouvert { remplace: None }
        ));
        donner(&mut m, &bureau, t1, 0, now);
        donner(&mut m, &salon, t2, 0, now);
        assert_eq!(m.fin(&salon, t2).unwrap().poste, salon);
        assert_eq!(m.nombre_ouverts(), 1);
    }

    #[test]
    fn un_tour_sans_son_depuis_trois_secondes_se_ferme() {
        let (mut m, p, t, debut) = (
            machine(),
            poste("bureau"),
            IdTour::nouveau(),
            Instant::now(),
        );
        ouvrir(&mut m, &p, t, debut);
        donner(&mut m, &p, t, 0, debut);
        let dernier = debut + Duration::from_secs(5);
        donner(&mut m, &p, t, 1, dernier);
        assert!(m.expirer(dernier + Duration::from_millis(2_999)).is_empty());
        let fermes = m.expirer(dernier + SILENCE_QUI_FERME_LE_TOUR);
        assert_eq!(fermes.len(), 1);
        assert_eq!((fermes[0].raison, fermes[0].tour), (Raison::Silence, t));
        assert_eq!(fermes[0].session, vec![0, 1]);
        // Fermé : une fin tardive, ou le son qui reprendrait, ne rouvrent rien.
        assert!(m.fin(&p, t).is_none());
        assert!(matches!(m.son(&p, t, 2, dernier), Son::Inconnu));
    }

    #[test]
    fn un_tour_qui_n_a_jamais_eu_de_son_expire_aussi() {
        let (mut m, p, t, debut) = (
            machine(),
            poste("bureau"),
            IdTour::nouveau(),
            Instant::now(),
        );
        ouvrir(&mut m, &p, t, debut);
        assert!(m.expirer(debut + Duration::from_secs(1)).is_empty());
        assert_eq!(m.expirer(debut + Duration::from_secs(3)).len(), 1);
    }

    #[test]
    fn l_arret_ferme_tout() {
        let (mut m, now) = (machine(), Instant::now());
        ouvrir(&mut m, &poste("bureau"), IdTour::nouveau(), now);
        ouvrir(&mut m, &poste("salon"), IdTour::nouveau(), now);
        let fermes = m.tout_fermer();
        assert_eq!(fermes.len(), 2);
        assert!(fermes.iter().all(|f| f.raison == Raison::Arret));
        assert_eq!(m.nombre_ouverts(), 0);
    }
}
