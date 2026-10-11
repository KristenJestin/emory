//! Vérifier la bouche de bout en bout sans poste : publie les `Dire` d'un faux tour sur un nom de
//! poste, écoute la voix qui revient sur `poste.<nom>.audio.retour` et l'écrit dans un WAV.
//!
//! ```text
//! bouche-essai [--nats-url URL] [--poste NOM] [--sortie voix.wav] [--stop-apres-la-premiere]
//!              [PHRASE…]
//! ```
//!
//! Avec `--stop-apres-la-premiere`, un `Stop` part dès que la première phrase est entièrement
//! reçue ; l'outil attend ensuite que plus rien n'arrive pour vérifier que le reste n'est pas dit.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use contract::{
    Appareil, Dire, IdTour, Parle, Stop,
    son::{Format, entetes},
    sujets,
};
use futures_util::StreamExt;

const PHRASES: [&str; 3] = [
    "Bonjour Kris, les oreilles sont spécifiées et la bouche parle enfin.",
    "Je peux dire plusieurs phrases à la suite, sans attendre que tu aies fini de lire.",
    "Et si tu m'interromps, je me tais tout de suite.",
];

struct Args {
    nats_url: String,
    poste: Appareil,
    output: PathBuf,
    stop_after_first: bool,
    sentences: Vec<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        nats_url: "nats://127.0.0.1:4222".to_owned(),
        poste: Appareil::new("essai")?,
        output: PathBuf::from("voix.wav"),
        stop_after_first: false,
        sentences: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .with_context(|| format!("{name} attend une valeur"))
        };
        match arg.as_str() {
            "--nats-url" => args.nats_url = value("--nats-url")?,
            "--poste" => args.poste = Appareil::new(value("--poste")?)?,
            "--sortie" => args.output = value("--sortie")?.into(),
            "--stop-apres-la-premiere" => args.stop_after_first = true,
            s if s.starts_with("--") => bail!("option inconnue : {s}"),
            _ => args.sentences.push(arg),
        }
    }
    if args.sentences.is_empty() {
        args.sentences = PHRASES.map(str::to_owned).to_vec();
    }
    Ok(args)
}

#[derive(Default)]
struct Sentence {
    bytes: usize,
    chunks: u32,
    first: Option<Duration>,
    done: Option<Duration>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let nats = bouche::nats::connect_to(&args.nats_url, None, None, "bouche-essai").await?;
    let mut voice = nats.subscribe(sujets::son_retour(&args.poste)).await?;
    let mut parle = nats.subscribe(sujets::parle(&args.poste)).await?;
    nats.flush().await?;

    let tour = IdTour::nouveau();
    let start = Instant::now();
    for (numero, texte) in args.sentences.iter().enumerate() {
        let dire = Dire {
            tour,
            poste: args.poste.clone(),
            numero: numero as u32,
            texte: texte.clone(),
        };
        nats.publish(sujets::DIRE, serde_json::to_vec(&dire)?.into())
            .await?;
    }
    nats.flush().await?;
    println!(
        "tour {tour} : {} phrases publiées sur {}",
        args.sentences.len(),
        args.poste
    );

    let mut sentences: Vec<Sentence> = args.sentences.iter().map(|_| Sentence::default()).collect();
    let mut pcm = Vec::new();
    let mut format: Option<Format> = None;
    let mut expected_seq = 0u32;
    let mut stopped = false;
    // Sans `--stop…` : fini quand la dernière phrase a son `Fin`. Avec : quand le silence dure.
    let quiet = Duration::from_secs(if args.stop_after_first { 4 } else { 20 });
    loop {
        let message = tokio::select! {
            m = voice.next() => m.context("le bus s'est fermé")?,
            Some(m) = parle.next() => {
                if let Ok(p) = serde_json::from_slice::<Parle>(&m.payload) {
                    println!("{:>7.0} ms  Parle {:?}", start.elapsed().as_secs_f64() * 1e3, p.etat);
                }
                continue;
            }
            () = tokio::time::sleep(quiet) => {
                if args.stop_after_first { break } else { bail!("plus de son depuis {quiet:?}") }
            }
        };
        let headers = message.headers.as_ref().context("morceau sans en-têtes")?;
        let header = |name: &str| headers.get(name).map(|v| v.as_str().to_owned());
        if header(entetes::TOUR).as_deref() != Some(&tour.to_string()) {
            continue;
        }
        let seq: u32 = header(entetes::SEQ).context("Emory-Seq manque")?.parse()?;
        if seq != expected_seq {
            println!("!! trou dans le son : attendu {expected_seq}, reçu {seq}");
        }
        expected_seq = seq + 1;
        let f: Format = header(entetes::FORMAT)
            .context("Emory-Format manque")?
            .parse()?;
        match format {
            Some(known) if known != f => bail!("le format change en cours de tour"),
            _ => format = Some(f),
        }
        let numero: usize = header(entetes::PHRASE)
            .context("Emory-Phrase manque")?
            .parse()?;
        let last = header(entetes::FIN).as_deref() == Some("1");
        let s = sentences
            .get_mut(numero)
            .context("numéro de phrase inconnu")?;
        let at = start.elapsed();
        if !message.payload.is_empty() {
            s.first.get_or_insert(at);
        }
        s.bytes += message.payload.len();
        s.chunks += 1;
        pcm.extend_from_slice(&message.payload);
        if last {
            s.done = Some(at);
            if args.stop_after_first && numero == 0 && !stopped {
                stopped = true;
                let stop = Stop {
                    tour,
                    poste: args.poste.clone(),
                };
                nats.publish(sujets::STOP, serde_json::to_vec(&stop)?.into())
                    .await?;
                nats.flush().await?;
                println!("{:>7.0} ms  Stop envoyé", at.as_secs_f64() * 1e3);
            }
            if !args.stop_after_first && numero == sentences.len() - 1 {
                break;
            }
        }
    }

    let format = format.context("aucun son reçu")?;
    write_wav(&args.output, &pcm, format)?;
    println!(
        "\nfichier : {} ({:.2} s de voix, {})",
        args.output.display(),
        seconds(pcm.len(), format),
        format
    );
    for (i, s) in sentences.iter().enumerate() {
        let audio = seconds(s.bytes, format);
        match (s.first, s.done) {
            (Some(first), Some(done)) => println!(
                "phrase {i} : premier son {:>4.0} ms après l'envoi, {} morceaux, {:.2} s de voix, finie à {:>5.0} ms ({}× plus vite que le temps réel)",
                first.as_secs_f64() * 1e3,
                s.chunks,
                audio,
                done.as_secs_f64() * 1e3,
                if done > first {
                    format!("{:.1}", audio / (done - first).as_secs_f64())
                } else {
                    "?".to_owned()
                },
            ),
            (None, None) if s.chunks == 0 => println!("phrase {i} : rien reçu"),
            _ => println!(
                "phrase {i} : {} morceaux, {:.2} s de voix, sans fin annoncée",
                s.chunks, audio
            ),
        }
    }
    if args.stop_after_first {
        let said = sentences.iter().skip(1).filter(|s| s.chunks > 0).count();
        println!(
            "\nStop : {}",
            if said == 0 {
                "les phrases suivantes ne sont pas dites".to_owned()
            } else {
                format!("ERREUR, {said} phrase(s) dite(s) malgré le Stop")
            }
        );
        if said > 0 {
            std::process::exit(1);
        }
    }
    Ok(())
}

fn seconds(bytes: usize, format: Format) -> f64 {
    bytes as f64 / (2.0 * format.canaux as f64 * format.frequence as f64)
}

fn write_wav(path: &std::path::Path, pcm: &[u8], format: Format) -> anyhow::Result<()> {
    let rate = format.frequence;
    let block = format.canaux * 2;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend(b"RIFF");
    wav.extend((36 + pcm.len() as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(format.canaux.to_le_bytes());
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * block as u32).to_le_bytes());
    wav.extend(block.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((pcm.len() as u32).to_le_bytes());
    wav.extend(pcm);
    std::fs::write(path, wav).with_context(|| format!("écriture de {}", path.display()))
}
