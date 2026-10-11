//! Les moteurs, par sherpa-onnx : Silero VAD pour découper les phrases, Parakeet TDT 0.6B v3 int8
//! pour les transcrire.
//!
//! Le détecteur est léger (12 ms de calcul par seconde de son au banc) : il tourne là où le son
//! arrive. La transcription est lourde : un fil à elle (voir [`Transcripteur`]), une phrase à la
//! fois, loin de l'exécuteur asynchrone.

use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::config::{ConfigModeles, ConfigTranscription, ConfigVad};

const FREQUENCE: i32 = 16_000;

/// Une phrase que le VAD a fermée.
pub struct Segment {
    /// L'échantillon, depuis le début du tour, où la phrase commence.
    pub debut: u64,
    pub echantillons: Vec<f32>,
}

impl Segment {
    pub fn duree(&self) -> Duration {
        Duration::from_secs_f64(self.echantillons.len() as f64 / FREQUENCE as f64)
    }
}

/// Silero VAD pour un tour : il reçoit le son au fil de l'eau et rend les phrases fermées.
pub struct Detecteur {
    vad: VoiceActivityDetector,
}

impl Detecteur {
    pub fn nouveau(modele: &Path, reglages: &ConfigVad) -> Result<Self> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(chemin(modele)?),
                threshold: reglages.seuil,
                min_silence_duration: reglages.silence_min,
                min_speech_duration: reglages.parole_min,
                window_size: 512,
                max_speech_duration: reglages.parole_max,
            },
            sample_rate: FREQUENCE,
            num_threads: 1,
            provider: Some("cpu".to_owned()),
            ..VadModelConfig::default()
        };
        let vad = VoiceActivityDetector::create(&config, 30.0)
            .ok_or_else(|| anyhow!("Silero VAD n'a pas démarré ({})", modele.display()))?;
        Ok(Self { vad })
    }

    /// Donne du son au détecteur ; rend les phrases qu'il vient de fermer.
    pub fn alimenter(&mut self, echantillons: &[f32]) -> Vec<Segment> {
        self.vad.accept_waveform(echantillons);
        self.recolter()
    }

    /// La fin du tour : la phrase en cours, si elle dure, est fermée de force.
    pub fn vider(&mut self) -> Vec<Segment> {
        self.vad.flush();
        self.recolter()
    }

    fn recolter(&mut self) -> Vec<Segment> {
        let mut segments = Vec::new();
        while let Some(segment) = self.vad.front() {
            segments.push(Segment {
                debut: segment.start().max(0) as u64,
                echantillons: segment.samples().to_vec(),
            });
            self.vad.pop();
        }
        segments
    }
}

/// Ce que rend une transcription.
#[derive(Debug)]
pub struct Transcription {
    pub texte: String,
    /// Le temps de calcul.
    pub duree: Duration,
    /// Le temps passé à attendre son tour avant le calcul.
    pub attente: Duration,
}

struct Travail {
    echantillons: Vec<f32>,
    depose: Instant,
    reponse: oneshot::Sender<Result<Transcription, String>>,
}

/// Parakeet sur un fil à lui : les phrases passent une à une, dans l'ordre où elles arrivent.
#[derive(Clone)]
pub struct Transcripteur {
    envoi: mpsc::Sender<Travail>,
}

impl Transcripteur {
    /// Charge Parakeet, le chauffe sur une seconde de silence, puis lance le fil.
    pub fn demarrer(modeles: &ConfigModeles, reglages: &ConfigTranscription) -> Result<Self> {
        let dossier = modeles.parakeet.as_ref();
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: Some(chemin(&dossier.join("encoder.int8.onnx"))?),
                    decoder: Some(chemin(&dossier.join("decoder.int8.onnx"))?),
                    joiner: Some(chemin(&dossier.join("joiner.int8.onnx"))?),
                },
                tokens: Some(chemin(&dossier.join("tokens.txt"))?),
                num_threads: reglages.fils.max(1),
                provider: Some("cpu".to_owned()),
                model_type: Some("nemo_transducer".to_owned()),
                ..OfflineModelConfig::default()
            },
            decoding_method: Some("greedy_search".to_owned()),
            ..OfflineRecognizerConfig::default()
        };
        let debut = Instant::now();
        let reconnaisseur = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("Parakeet n'a pas démarré ({})", dossier.display()))?;
        let charge = debut.elapsed();
        let chauffe = Instant::now();
        transcrire(&reconnaisseur, &vec![0.0; FREQUENCE as usize])?;
        tracing::info!(
            fils = reglages.fils,
            chargement_ms = charge.as_millis() as u64,
            chauffe_ms = chauffe.elapsed().as_millis() as u64,
            "Parakeet prêt"
        );

        let (envoi, reception) = mpsc::channel::<Travail>();
        std::thread::Builder::new()
            .name("transcription".to_owned())
            .spawn(move || {
                for travail in reception {
                    let attente = travail.depose.elapsed();
                    let debut = Instant::now();
                    let resultat = transcrire(&reconnaisseur, &travail.echantillons)
                        .map(|texte| Transcription {
                            texte,
                            duree: debut.elapsed(),
                            attente,
                        })
                        .map_err(|e| format!("{e:#}"));
                    if travail.reponse.send(resultat).is_err() {
                        debug!("transcription terminée, mais plus personne ne l'attend");
                    }
                }
            })
            .context("fil de transcription")?;
        Ok(Self { envoi })
    }

    /// Dépose une phrase ; la réponse arrive quand son tour est venu.
    pub fn transcrire(
        &self,
        echantillons: Vec<f32>,
    ) -> oneshot::Receiver<Result<Transcription, String>> {
        let (reponse, attente) = oneshot::channel();
        let travail = Travail {
            echantillons,
            depose: Instant::now(),
            reponse,
        };
        if self.envoi.send(travail).is_err() {
            error!("le fil de transcription s'est arrêté");
        }
        attente
    }
}

fn transcrire(reconnaisseur: &OfflineRecognizer, echantillons: &[f32]) -> Result<String> {
    let flux = reconnaisseur.create_stream();
    flux.accept_waveform(FREQUENCE, echantillons);
    reconnaisseur.decode(&flux);
    let resultat = flux
        .get_result()
        .ok_or_else(|| anyhow!("Parakeet n'a rien rendu"))?;
    Ok(resultat.text.trim().to_owned())
}

fn chemin(chemin: &Path) -> Result<String> {
    if !chemin.exists() {
        anyhow::bail!(
            "{} n'existe pas (voir scripts/telecharger-modeles.sh)",
            chemin.display()
        );
    }
    chemin
        .to_str()
        .map(str::to_owned)
        .with_context(|| format!("chemin non UTF-8 : {}", chemin.display()))
}
