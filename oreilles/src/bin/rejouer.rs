//! `rejouer [--nats URL] [--ca PEM] [--nkey GRAINE] [--poste NOM] [--sans-fin] fichier.wav`
//!
//! Joue un fichier WAV sur le bus comme le ferait un poste (`debut`, morceaux de 20 ms au rythme
//! du temps réel, `fin` ; avec `--sans-fin`, le poste « meurt » sans l'envoyer), et affiche les phrases et le tour que les oreilles renvoient. C'est
//! l'essai de bout en bout des oreilles. Le fichier doit être en 16 kHz mono 16 bits :
//! `ffmpeg -i entree.wav -ar 16000 -ac 1 -c:a pcm_s16le sortie.wav`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_nats::HeaderMap;
use contract::{Appareil, Bouton, Etat, IdTour, Phrase, Tour, son, sujets};
use futures_util::StreamExt;
use oreilles::{
    bus::{self, publier},
    config::{Chemin, ConfigNats},
};
use tokio::time::sleep_until;

/// Combien de temps on attend le tour après la fin.
const DELAI_TOUR: Duration = Duration::from_secs(60);

struct Arguments {
    nats: ConfigNats,
    poste: Appareil,
    fichier: String,
    sans_fin: bool,
}

fn arguments() -> Result<Arguments> {
    let mut nats = ConfigNats::default();
    let mut poste = "bureau".parse()?;
    let mut fichier = None;
    let mut sans_fin = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut valeur = |nom: &str| {
            args.next()
                .with_context(|| format!("{nom} attend une valeur"))
        };
        match arg.as_str() {
            "--nats" => nats.url = valeur("--nats")?,
            "--ca" => nats.ca = Some(Chemin(valeur("--ca")?.into())),
            "--nkey" => nats.nkey = Some(Chemin(valeur("--nkey")?.into())),
            "--poste" => poste = valeur("--poste")?.parse()?,
            "--sans-fin" => sans_fin = true,
            _ if arg.starts_with("--") => bail!("option inconnue : {arg}"),
            _ => fichier = Some(arg),
        }
    }
    let fichier = fichier.context(
        "usage : rejouer [--nats URL] [--ca PEM] [--nkey GRAINE] [--poste NOM] [--sans-fin] fichier.wav",
    )?;
    Ok(Arguments {
        nats,
        poste,
        fichier,
        sans_fin,
    })
}

fn lire_wav(chemin: &str) -> Result<Vec<i16>> {
    let mut lecteur =
        hound::WavReader::open(chemin).with_context(|| format!("ouverture de {chemin}"))?;
    let spec = lecteur.spec();
    if spec.sample_rate != son::MICRO.frequence
        || spec.channels != son::MICRO.canaux
        || spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
    {
        bail!(
            "{chemin} : {} Hz, {} canal, {} bits ; il faut 16 kHz mono 16 bits \
             (ffmpeg -i entree.wav -ar 16000 -ac 1 -c:a pcm_s16le sortie.wav)",
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample
        );
    }
    Ok(lecteur.samples::<i16>().collect::<Result<_, _>>()?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = arguments()?;
    let son_wav = lire_wav(&args.fichier)?;
    let morceau = son::OCTETS_MORCEAU_MICRO / 2;
    println!(
        "{} : {:.1} s, {} morceaux de 20 ms",
        args.fichier,
        son_wav.len() as f64 / son::MICRO.frequence as f64,
        son_wav.len().div_ceil(morceau)
    );

    let client = bus::connecter(&args.nats, "rejouer", false).await?;
    let tour = IdTour::nouveau();
    let mut phrases = client.subscribe(sujets::PHRASE).await?;
    let mut tours = client.subscribe(sujets::TOUR).await?;
    client.flush().await?;

    let debut = Instant::now();
    let horloge = move || format!("{:>6.2} s", debut.elapsed().as_secs_f64());

    // Ce que les oreilles renvoient, affiché au fur et à mesure.
    let ecoute = tokio::spawn({
        async move {
            loop {
                tokio::select! {
                    biased;
                    Some(m) = phrases.next() => {
                        let Ok(p) = serde_json::from_slice::<Phrase>(&m.payload) else { continue };
                        if p.tour != tour { continue }
                        println!("{}  phrase {} [{} → {}] : {}", horloge(), p.numero,
                            p.debut.format("%H:%M:%S%.3f"), p.fin.format("%H:%M:%S%.3f"), p.texte);
                    }
                    Some(m) = tours.next() => {
                        let Ok(t) = serde_json::from_slice::<Tour>(&m.payload) else { continue };
                        if t.tour != tour { continue }
                        return Ok((t, Instant::now()));
                    }
                    else => bail!("le bus s'est fermé"),
                }
            }
        }
    });

    let bouton = |etat| Bouton {
        tour,
        etat,
        quand: contract::maintenant(),
    };
    publier(&client, sujets::bouton(&args.poste), &bouton(Etat::Debut)).await?;
    println!("{}  début du tour {tour}", horloge());

    let sujet_son = sujets::son(&args.poste);
    let depart = tokio::time::Instant::now();
    for (seq, echantillons) in son_wav.chunks(morceau).enumerate() {
        sleep_until(depart + son::MORCEAU_MICRO * seq as u32).await;
        let mut entetes = HeaderMap::new();
        entetes.insert(son::entetes::TOUR, tour.to_string());
        entetes.insert(son::entetes::SEQ, seq.to_string());
        entetes.insert(son::entetes::FORMAT, son::MICRO.to_string());
        let octets: Vec<u8> = echantillons.iter().flat_map(|e| e.to_le_bytes()).collect();
        client
            .publish_with_headers(sujet_son.clone(), entetes, octets.into())
            .await?;
    }
    sleep_until(depart + son::MORCEAU_MICRO * son_wav.len().div_ceil(morceau) as u32).await;
    let instant_fin = Instant::now();
    if args.sans_fin {
        println!("{}  le poste meurt : pas de fin", horloge());
    } else {
        publier(&client, sujets::bouton(&args.poste), &bouton(Etat::Fin)).await?;
        println!("{}  fin du tour", horloge());
    }

    let (tour_recu, recu) = tokio::time::timeout(DELAI_TOUR, ecoute)
        .await
        .context("pas de tour reçu")???;
    println!("{}  tour : {:?}", horloge(), tour_recu.texte);
    println!(
        "locuteur {:?}, réveil {:?}",
        tour_recu.locuteur.map(|l| (l.voix, l.origine)),
        tour_recu.reveil
    );
    println!(
        "de la fin au tour : {:.0} ms",
        recu.duration_since(instant_fin).as_secs_f64() * 1000.0
    );
    Ok(())
}
