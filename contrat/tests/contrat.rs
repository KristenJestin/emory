//! Les exemples de `specs/contrat.md` se lisent et se réécrivent à l'identique.

use contrat::{son, sujets, *};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn aller_retour<T: Serialize + DeserializeOwned>(exemple: Value) -> T {
    let message: T = serde_json::from_value(exemple.clone()).expect("le message se lit");
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        exemple,
        "le message se réécrit à l'identique"
    );
    message
}

const TOUR: &str = "01K7A3C2Z9W8V6T5S4R3Q2P1N0";
const QUAND: &str = "2026-10-11T21:14:03.120+02:00";

#[test]
fn bouton() {
    let b: Bouton = aller_retour(json!({ "tour": TOUR, "etat": "debut", "quand": QUAND }));
    assert_eq!(b.etat, Etat::Debut);
    assert_eq!(b.tour.to_string(), TOUR);
}

#[test]
fn phrase() {
    aller_retour::<Phrase>(json!({
        "tour": TOUR, "poste": "bureau", "numero": 0, "texte": "on en est où sur emory",
        "debut": QUAND, "fin": QUAND
    }));
}

#[test]
fn tour_de_la_version_1() {
    let t: Tour = aller_retour(json!({
        "tour": TOUR, "poste": "bureau", "debut": QUAND, "fin": QUAND,
        "texte": "on en est où sur emory et les oreilles", "reveil": "bouton",
        "locuteur": null, "contexte": []
    }));
    assert!(t.locuteur.is_none());
}

#[test]
fn tour_complet_de_oreilles_md() {
    aller_retour::<Tour>(json!({
        "tour": TOUR, "poste": "bureau", "debut": QUAND, "fin": QUAND,
        "texte": "Emory, rappelle-moi ce qu'on avait décidé pour la musique",
        "reveil": "mot",
        "locuteur": { "voix": "kris", "personne": "kris", "origine": "connue", "confiance": 0.5 },
        "confiance_transcription": 0.25,
        "contexte": [{ "debut": QUAND, "voix": "voix-7", "personne": null, "texte": "…" }]
    }));
}

#[test]
fn un_champ_inconnu_est_ignore_et_un_champ_absent_prend_sa_valeur_par_defaut() {
    let t: Tour = serde_json::from_value(json!({
        "tour": TOUR, "poste": "bureau", "debut": QUAND, "fin": QUAND, "texte": "",
        "reveil": "bouton", "champ_de_demain": 3
    }))
    .unwrap();
    assert!(t.locuteur.is_none() && t.contexte.is_empty());
}

#[test]
fn dire_stop_parle() {
    aller_retour::<Dire>(
        json!({ "tour": TOUR, "poste": "bureau", "numero": 0, "texte": "Les oreilles sont spécifiées." }),
    );
    aller_retour::<Stop>(json!({ "tour": TOUR, "poste": "bureau" }));
    aller_retour::<Parle>(json!({ "etat": "fin" }));
}

#[test]
fn annonces() {
    aller_retour::<Annonce>(json!({
        "appareil": "bureau", "programme": "main", "version": "0.1.0", "modules": ["mpris"],
        "actions": [{ "nom": "mpris.pause", "description": "Met la musique en pause.", "arguments": {} }]
    }));
    let poste: Annonce = serde_json::from_value(json!({
        "appareil": "bureau", "programme": "poste", "version": "0.1.0", "modules": ["micro", "haut-parleur"]
    }))
    .unwrap();
    assert!(poste.actions.is_empty());
}

#[test]
fn reponses_d_action() {
    let ok: ReponseAction = aller_retour(
        json!({ "ok": true, "resultat": { "titre": "One More Time", "artiste": "Daft Punk" } }),
    );
    assert_eq!(ok.en_result().unwrap()["artiste"], "Daft Punk");
    let rate: ReponseAction =
        aller_retour(json!({ "ok": false, "erreur": "aucun lecteur ouvert" }));
    assert_eq!(rate.en_result().unwrap_err(), "aucun lecteur ouvert");
}

#[test]
fn un_nom_d_appareil_invalide_est_refuse() {
    for nom in ["", "Bureau", "bureau.salon", "bu reau", "*", ">"] {
        assert!(Appareil::new(nom).is_err(), "{nom:?}");
    }
    assert!(serde_json::from_value::<Stop>(json!({ "tour": TOUR, "poste": "a.b" })).is_err());
    assert!(Appareil::new("salon-2").is_ok());
}

#[test]
fn les_sujets() {
    let bureau = Appareil::new("bureau").unwrap();
    assert_eq!(sujets::bouton(&bureau), "poste.bureau.bouton");
    assert_eq!(sujets::son(&bureau), "poste.bureau.audio");
    assert_eq!(sujets::son_retour(&bureau), "poste.bureau.audio.retour");
    assert_eq!(sujets::annonce_poste(&bureau), "poste.bureau.annonce");
    assert_eq!(sujets::annonce_main(&bureau), "main.bureau.annonce");
    assert_eq!(sujets::parle(&bureau), "bouche.bureau.parle");
    assert_eq!(
        sujets::action(&bureau, "mpris.pause").as_deref(),
        Some("main.bureau.mpris.pause")
    );
    assert_eq!(sujets::action(&bureau, "annonce"), None);
    assert_eq!(sujets::action(&bureau, "mpris..pause"), None);
    assert_eq!(sujets::action(&bureau, "mpris.*"), None);

    assert_eq!(
        sujets::appareil_du_sujet("poste.bureau.audio.retour"),
        Some(bureau.clone())
    );
    assert_eq!(
        sujets::appareil_du_sujet("main.bureau.mpris.pause"),
        Some(bureau.clone())
    );
    assert_eq!(sujets::appareil_du_sujet("oreilles.tour"), None);
    assert_eq!(
        sujets::action_du_sujet("main.bureau.mpris.pause"),
        Some("mpris.pause")
    );
    assert_eq!(sujets::action_du_sujet("main.bureau.annonce"), None);
}

#[test]
fn le_format_du_son() {
    assert_eq!(son::MICRO.to_string(), "s16le;16000;1");
    assert_eq!(
        "s16le;24000;1".parse::<son::Format>().unwrap().frequence,
        24_000
    );
    for faux in [
        "",
        "s16le",
        "f32le;16000;1",
        "s16le;0;1",
        "s16le;16000;1;x",
        "s16le;seize;1",
    ] {
        assert!(faux.parse::<son::Format>().is_err(), "{faux:?}");
    }
    assert_eq!(
        son::MICRO.octets(son::MORCEAU_MICRO),
        son::OCTETS_MORCEAU_MICRO
    );
}

#[test]
fn chaque_tour_a_son_identifiant() {
    assert_ne!(IdTour::nouveau(), IdTour::nouveau());
    let id = IdTour::nouveau();
    assert_eq!(id.to_string().parse::<IdTour>().unwrap(), id);
}
