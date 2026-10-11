//! Le service : les messages du bus entrent, les phrases et les tours transcrits sortent.
//!
//! Une seule tâche suit le bus et la machine des tours ; la transcription est ailleurs (voir
//! [`crate::moteur`]). Chaque tour a sa tâche de publication, qui reçoit les phrases dans l'ordre
//! où elles ont été fermées, attend leur transcription, les publie, puis publie le tour.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use async_nats::Client;
use chrono::TimeDelta;
use contract::{
    Appareil, Bouton, Etat, IdTour, Locuteur, Origine, Phrase, Quand, Reveil, Tour, son, sujets,
};
use futures_util::StreamExt;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::MissedTickBehavior,
};
use tracing::{debug, info, warn};

use crate::{
    audio,
    bus::publier,
    config::{Chemin, ConfigVad},
    corrections::Corrections,
    moteur::{Detecteur, Segment, Transcripteur, Transcription},
    tours::{Debut, Ferme, Postes, Raison, Son},
};

/// Le rythme auquel on regarde si un tour s'est tu.
const GUET: Duration = Duration::from_millis(250);
/// Ce qu'on attend, à l'arrêt, que les tours en cours finissent de partir.
const DELAI_ARRET: Duration = Duration::from_secs(10);

/// Une phrase déposée au transcripteur, dont on attend le texte.
struct Attente {
    debut: Quand,
    fin: Quand,
    duree: Duration,
    reponse: oneshot::Receiver<Result<Transcription, String>>,
}

enum Message {
    Phrase(Attente),
    Fin {
        fin: Quand,
        instant: Instant,
        raison: Raison,
    },
}

/// Un tour en cours : son détecteur, et la file vers la tâche qui publie.
struct Session {
    detecteur: Detecteur,
    transcripteur: Transcripteur,
    file: mpsc::UnboundedSender<Message>,
    debut: Quand,
    echantillons: u64,
}

impl Session {
    fn alimenter(&mut self, son: &[f32]) {
        self.echantillons += son.len() as u64;
        let segments = self.detecteur.alimenter(son);
        self.envoyer(segments);
    }

    /// Ferme le tour : la phrase en cours est transcrite à son tour, puis le tour part.
    fn fermer(mut self, fin: Quand, instant: Instant, raison: Raison) {
        let segments = self.detecteur.vider();
        self.envoyer(segments);
        let _ = self.file.send(Message::Fin {
            fin,
            instant,
            raison,
        });
    }

    fn envoyer(&self, segments: Vec<Segment>) {
        for segment in segments {
            let depart = |echantillons: u64| {
                self.debut + TimeDelta::microseconds((echantillons * 1_000_000 / 16_000) as i64)
            };
            let duree = segment.duree();
            let attente = Attente {
                debut: depart(segment.debut),
                fin: depart(segment.debut + segment.echantillons.len() as u64),
                duree,
                reponse: self.transcripteur.transcrire(segment.echantillons),
            };
            let _ = self.file.send(Message::Phrase(attente));
        }
    }
}

/// Un avertissement par poste et par tour, pas un par morceau de son (50 par seconde).
#[derive(Default)]
struct Avertissements(HashMap<(Appareil, &'static str), IdTour>);

impl Avertissements {
    fn premiere_fois(&mut self, poste: &Appareil, genre: &'static str, tour: IdTour) -> bool {
        self.0.insert((poste.clone(), genre), tour) != Some(tour)
    }
}

pub struct Service {
    pub client: Client,
    pub vad: Chemin,
    pub reglages_vad: ConfigVad,
    pub transcripteur: Transcripteur,
    pub corrections: Arc<Corrections>,
}

impl Service {
    /// Écoute le bus jusqu'à l'arrêt du programme (Ctrl-C ou SIGTERM).
    pub async fn executer(self) -> Result<()> {
        let mut boutons = self.client.subscribe(sujets::TOUS_LES_BOUTONS).await?;
        let mut sons = self.client.subscribe(sujets::TOUS_LES_SONS).await?;
        info!(
            boutons = sujets::TOUS_LES_BOUTONS,
            sons = sujets::TOUS_LES_SONS,
            "oreilles à l'écoute"
        );

        let mut postes = Postes::<Session>::new(son::SILENCE_QUI_FERME_LE_TOUR);
        let mut publications = JoinSet::new();
        let mut avertis = Avertissements::default();
        let mut guet = tokio::time::interval(GUET);
        guet.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let arret = arret();
        tokio::pin!(arret);

        loop {
            tokio::select! {
                Some(message) = boutons.next() => {
                    self.bouton(&mut postes, &mut publications, message);
                }
                Some(message) = sons.next() => {
                    traiter_son(&mut postes, &mut avertis, message);
                }
                _ = guet.tick() => {
                    for ferme in postes.expirer(Instant::now()) {
                        warn!(poste = %ferme.poste, tour = %ferme.tour, "plus de son : le tour est fermé");
                        fermer(ferme, contract::maintenant(), Instant::now());
                    }
                    while publications.try_join_next().is_some() {}
                }
                _ = &mut arret => break,
                else => break,
            }
        }

        info!("arrêt : fermeture des tours en cours");
        for ferme in postes.tout_fermer() {
            fermer(ferme, contract::maintenant(), Instant::now());
        }
        let fini = tokio::time::timeout(DELAI_ARRET, async {
            while publications.join_next().await.is_some() {}
        })
        .await;
        if fini.is_err() {
            warn!("des tours n'ont pas fini de partir");
        }
        Ok(())
    }

    fn bouton(
        &self,
        postes: &mut Postes<Session>,
        publications: &mut JoinSet<()>,
        message: async_nats::Message,
    ) {
        let Some(poste) = sujets::appareil_du_sujet(&message.subject) else {
            warn!(sujet = %message.subject, "bouton sur un sujet sans poste");
            return;
        };
        let bouton: Bouton = match serde_json::from_slice(&message.payload) {
            Ok(bouton) => bouton,
            Err(erreur) => {
                warn!(%poste, %erreur, "bouton illisible");
                return;
            }
        };
        let maintenant = Instant::now();
        match bouton.etat {
            Etat::Debut => {
                let (file, reception) = mpsc::unbounded_channel();
                let session =
                    Detecteur::nouveau(self.vad.as_ref(), &self.reglages_vad).map(|detecteur| {
                        Session {
                            detecteur,
                            transcripteur: self.transcripteur.clone(),
                            file,
                            debut: bouton.quand,
                            echantillons: 0,
                        }
                    });
                let session = match session {
                    Ok(session) => session,
                    Err(erreur) => {
                        warn!(%poste, tour = %bouton.tour, "{erreur:#}");
                        return;
                    }
                };
                match postes.debut(&poste, bouton.tour, bouton.quand, maintenant, || session) {
                    Debut::Doublon => debug!(%poste, tour = %bouton.tour, "début déjà reçu"),
                    Debut::Ouvert { remplace } => {
                        if let Some(ancien) = remplace {
                            warn!(%poste, tour = %ancien.tour, "nouveau début : l'ancien tour est fermé");
                            fermer(ancien, contract::maintenant(), maintenant);
                        }
                        info!(%poste, tour = %bouton.tour, "début du tour");
                        publications.spawn(publier_tour(
                            self.client.clone(),
                            poste,
                            bouton.tour,
                            bouton.quand,
                            reception,
                            self.corrections.clone(),
                        ));
                    }
                }
            }
            Etat::Fin => match postes.fin(&poste, bouton.tour) {
                Some(ferme) => fermer(ferme, bouton.quand, maintenant),
                None => debug!(%poste, tour = %bouton.tour, "fin d'un tour qui n'est pas ouvert"),
            },
        }
    }
}

fn fermer(ferme: Ferme<Session>, fin: Quand, instant: Instant) {
    ferme.session.fermer(fin, instant, ferme.raison);
}

fn traiter_son(
    postes: &mut Postes<Session>,
    avertis: &mut Avertissements,
    message: async_nats::Message,
) {
    let Some(poste) = sujets::appareil_du_sujet(&message.subject) else {
        return;
    };
    let entete = match audio::lire_entetes(message.headers.as_ref()) {
        Ok(entete) => entete,
        Err(erreur) => {
            warn!(%poste, "son jeté : {erreur:#}");
            return;
        }
    };
    let echantillons = match audio::echantillons(&message.payload) {
        Ok(e) => e,
        Err(erreur) => {
            warn!(%poste, tour = %entete.tour, "son jeté : {erreur:#}");
            return;
        }
    };
    match postes.son(&poste, entete.tour, entete.seq, Instant::now()) {
        Son::Accepte { session, trou } => {
            if let Some(trou) = trou {
                warn!(
                    %poste, tour = %entete.tour,
                    attendu = trou.attendu, recu = trou.recu,
                    manquants = trou.recu - trou.attendu,
                    "trou dans le son"
                );
            }
            session.alimenter(&echantillons);
        }
        Son::Ancien => {
            if avertis.premiere_fois(&poste, "ancien", entete.tour) {
                warn!(%poste, tour = %entete.tour, seq = entete.seq, "morceau de son en double ou en retard : jeté");
            }
        }
        Son::Inconnu => {
            if avertis.premiere_fois(&poste, "inconnu", entete.tour) {
                warn!(%poste, tour = %entete.tour, "son d'un tour qui n'est pas ouvert : jeté");
            }
        }
    }
}

/// La tâche d'un tour : publie ses phrases dans l'ordre, puis le tour.
async fn publier_tour(
    client: Client,
    poste: Appareil,
    tour: IdTour,
    debut: Quand,
    mut file: mpsc::UnboundedReceiver<Message>,
    corrections: Arc<Corrections>,
) {
    let mut textes: Vec<String> = Vec::new();
    let mut fermeture = None;
    while let Some(message) = file.recv().await {
        match message {
            Message::Phrase(attente) => {
                let Attente {
                    debut,
                    fin,
                    duree,
                    reponse,
                } = attente;
                let numero = textes.len() as u32;
                let transcription = match reponse.await {
                    Ok(Ok(transcription)) => transcription,
                    Ok(Err(erreur)) => {
                        warn!(%poste, %tour, "phrase non transcrite : {erreur}");
                        continue;
                    }
                    Err(_) => {
                        warn!(%poste, %tour, "phrase non transcrite : le fil de transcription s'est arrêté");
                        continue;
                    }
                };
                let texte = corrections.appliquer(&transcription.texte);
                info!(
                    %poste, %tour, numero,
                    audio_ms = duree.as_millis() as u64,
                    transcription_ms = transcription.duree.as_millis() as u64,
                    attente_ms = transcription.attente.as_millis() as u64,
                    caracteres = texte.chars().count(),
                    "phrase transcrite"
                );
                if texte.trim().is_empty() {
                    debug!(%poste, %tour, "phrase sans texte : jetée");
                    continue;
                }
                let phrase = Phrase {
                    tour,
                    poste: poste.clone(),
                    numero,
                    texte: texte.clone(),
                    debut,
                    fin,
                };
                if let Err(erreur) = publier(&client, sujets::PHRASE, &phrase).await {
                    warn!(%poste, %tour, "phrase non publiée : {erreur:#}");
                }
                textes.push(texte);
            }
            Message::Fin {
                fin,
                instant,
                raison,
            } => {
                fermeture = Some((fin, instant, raison));
                break;
            }
        }
    }
    let (fin, instant, raison) =
        fermeture.unwrap_or_else(|| (contract::maintenant(), Instant::now(), Raison::Arret));
    let phrases = textes.len();
    let message = Tour {
        tour,
        poste: poste.clone(),
        debut,
        fin,
        texte: textes.join(" "),
        reveil: Reveil::Bouton,
        locuteur: Some(Locuteur {
            voix: "kris".to_owned(),
            personne: Some("kris".to_owned()),
            origine: Origine::Connue,
            confiance: 1.0,
        }),
        confiance_transcription: None,
        contexte: Vec::new(),
    };
    let resultat = publier(&client, sujets::TOUR, &message).await;
    let fin_vers_tour = instant.elapsed();
    match resultat {
        Ok(()) => info!(
            %poste, %tour, ?raison, phrases,
            caracteres = message.texte.chars().count(),
            fin_vers_tour_ms = fin_vers_tour.as_millis() as u64,
            "tour publié"
        ),
        Err(erreur) => warn!(%poste, %tour, "tour non publié : {erreur:#}"),
    }
}

async fn arret() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terme = signal(SignalKind::terminate()).expect("signal SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terme.recv() => {}
    }
}
