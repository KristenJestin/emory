//! The examples of `specs/contract.md` are read and written back identically.

use contract::{audio, subjects, *};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn round_trip<T: Serialize + DeserializeOwned>(example: Value) -> T {
    let message: T = serde_json::from_value(example.clone()).expect("the message reads");
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        example,
        "the message is written back identically"
    );
    message
}

const TURN: &str = "01K7A3C2Z9W8V6T5S4R3Q2P1N0";
const AT: &str = "2026-10-11T21:14:03.120+02:00";

#[test]
fn button() {
    let b: Button = round_trip(json!({ "turn": TURN, "state": "start", "at": AT }));
    assert_eq!(b.state, State::Start);
    assert_eq!(b.turn.to_string(), TURN);
}

#[test]
fn sentence() {
    round_trip::<Sentence>(json!({
        "turn": TURN, "device": "office", "index": 0, "text": "on en est où sur emory",
        "start": AT, "end": AT
    }));
}

#[test]
fn turn_of_version_1() {
    let t: Turn = round_trip(json!({
        "turn": TURN, "device": "office", "start": AT, "end": AT,
        "text": "on en est où sur emory et les oreilles", "wake": "button",
        "speaker": null, "context": []
    }));
    assert!(t.speaker.is_none());
}

#[test]
fn full_turn_of_hearing_md() {
    round_trip::<Turn>(json!({
        "turn": TURN, "device": "office", "start": AT, "end": AT,
        "text": "Emory, rappelle-moi ce qu'on avait décidé pour la musique",
        "wake": "word",
        "speaker": { "voice": "alex", "person": "alex", "origin": "known", "confidence": 0.5 },
        "transcription_confidence": 0.25,
        "context": [{ "start": AT, "voice": "voix-7", "person": null, "text": "…" }]
    }));
}

#[test]
fn an_unknown_field_is_ignored_and_a_missing_field_takes_its_default() {
    let t: Turn = serde_json::from_value(json!({
        "turn": TURN, "device": "office", "start": AT, "end": AT, "text": "",
        "wake": "button", "field_of_tomorrow": 3
    }))
    .unwrap();
    assert!(t.speaker.is_none() && t.context.is_empty());
}

#[test]
fn say_stop_speaking() {
    round_trip::<Say>(json!({
        "turn": TURN, "device": "office", "index": 0, "text": "Les oreilles sont spécifiées.",
        "last": false
    }));
    round_trip::<Say>(json!({
        "turn": TURN, "device": "office", "index": 1, "text": "Et la bouche aussi.", "last": true
    }));
    round_trip::<Stop>(json!({ "turn": TURN, "device": "office" }));
    round_trip::<Speaking>(json!({ "state": "end" }));
}

#[test]
fn cues() {
    round_trip::<Cue>(json!({ "turn": TURN, "device": "office", "kind": "tone" }));
    round_trip::<Cue>(json!({ "turn": TURN, "device": "office", "kind": "pulse" }));
    round_trip::<Cue>(json!({
        "turn": TURN, "device": "office", "kind": "speech", "text": "Je regarde."
    }));
    assert_eq!(subjects::CUE, "voice.cue");
}

#[test]
fn say_without_last_is_not_the_last() {
    let say: Say = serde_json::from_value(json!({
        "turn": TURN, "device": "office", "index": 0, "text": "Bonjour."
    }))
    .unwrap();
    assert!(!say.last);
}

#[test]
fn announces() {
    round_trip::<Announce>(json!({
        "device": "office", "program": "hands", "version": "0.1.0", "modules": ["mpris"],
        "actions": [{ "name": "mpris.pause", "description": "Met la musique en pause.", "arguments": {} }]
    }));
    let ear: Announce = serde_json::from_value(json!({
        "device": "office", "program": "ear", "version": "0.1.0", "modules": ["microphone", "shortcut"]
    }))
    .unwrap();
    assert!(ear.actions.is_empty());
    round_trip::<Announce>(json!({
        "device": "office", "program": "mouth", "version": "0.1.0", "modules": ["speaker"],
        "actions": []
    }));
}

#[test]
fn action_replies() {
    let ok: ActionReply = round_trip(
        json!({ "ok": true, "result": { "title": "One More Time", "artist": "Daft Punk" } }),
    );
    assert_eq!(ok.into_result().unwrap()["artist"], "Daft Punk");
    let failed: ActionReply = round_trip(json!({ "ok": false, "error": "aucun lecteur ouvert" }));
    assert_eq!(failed.into_result().unwrap_err(), "aucun lecteur ouvert");
}

#[test]
fn vocabulary() {
    let add: VocabularyAdd = round_trip(json!({ "word": "Grafana", "heard_as": "gras fana" }));
    assert_eq!(add.heard_as.as_deref(), Some("gras fana"));
    let bare: VocabularyAdd = round_trip(json!({ "word": "Grafana" }));
    assert_eq!(bare.heard_as, None);
    let added: VocabularyReply = round_trip(json!({ "ok": true, "known": false, "in_use": true }));
    assert_eq!(added, VocabularyReply::success(false, true));
    let refused: VocabularyReply =
        round_trip(json!({ "ok": false, "known": false, "in_use": false, "error": "too long" }));
    assert_eq!(refused, VocabularyReply::failure("too long"));
    // An older hearing, or a shorter reply, still reads.
    let short: VocabularyReply = serde_json::from_value(json!({ "ok": true })).unwrap();
    assert!(short.ok && !short.known && !short.in_use);
    assert_eq!(subjects::VOCABULARY_ADD, "hearing.vocabulary.add");
}

#[test]
fn an_invalid_device_name_is_refused() {
    for name in ["", "Office", "office.living", "of fice", "*", ">"] {
        assert!(Device::new(name).is_err(), "{name:?}");
    }
    assert!(serde_json::from_value::<Stop>(json!({ "turn": TURN, "device": "a.b" })).is_err());
    assert!(Device::new("living-2").is_ok());
}

#[test]
fn the_subjects() {
    let office = Device::new("office").unwrap();
    assert_eq!(subjects::button(&office), "ear.office.button");
    assert_eq!(subjects::ear_audio(&office), "ear.office.audio");
    assert_eq!(subjects::ear_announce(&office), "ear.office.announce");
    assert_eq!(subjects::mouth_audio(&office), "mouth.office.audio");
    assert_eq!(subjects::mouth_announce(&office), "mouth.office.announce");
    assert_eq!(subjects::hands_announce(&office), "hands.office.announce");
    assert_eq!(subjects::speaking(&office), "voice.office.speaking");
    assert_eq!(
        subjects::action(&office, "mpris.pause").as_deref(),
        Some("hands.office.mpris.pause")
    );
    assert_eq!(subjects::action(&office, "announce"), None);
    assert_eq!(subjects::action(&office, "mpris..pause"), None);
    assert_eq!(subjects::action(&office, "mpris.*"), None);

    assert_eq!(
        subjects::device_of_subject("ear.office.audio"),
        Some(office.clone())
    );
    assert_eq!(
        subjects::device_of_subject("mouth.office.audio"),
        Some(office.clone())
    );
    assert_eq!(
        subjects::device_of_subject("hands.office.mpris.pause"),
        Some(office.clone())
    );
    assert_eq!(
        subjects::device_of_subject("voice.office.speaking"),
        Some(office.clone())
    );
    assert_eq!(subjects::device_of_subject("hearing.turn"), None);
    // `say` and `stop` are not devices: only `voice.<device>.speaking` has one.
    assert_eq!(subjects::device_of_subject("voice.say"), None);
    assert_eq!(subjects::device_of_subject("voice.stop"), None);
    assert_eq!(subjects::device_of_subject("voice.office.other"), None);
    assert_eq!(
        subjects::action_of_subject("hands.office.mpris.pause"),
        Some("mpris.pause")
    );
    assert_eq!(subjects::action_of_subject("hands.office.announce"), None);
}

#[test]
fn the_audio_format() {
    assert_eq!(audio::MIC.to_string(), "s16le;16000;1");
    assert_eq!(
        "s16le;24000;1"
            .parse::<audio::Format>()
            .unwrap()
            .sample_rate,
        24_000
    );
    for wrong in [
        "",
        "s16le",
        "f32le;16000;1",
        "s16le;0;1",
        "s16le;16000;1;x",
        "s16le;seize;1",
    ] {
        assert!(wrong.parse::<audio::Format>().is_err(), "{wrong:?}");
    }
    assert_eq!(audio::MIC.bytes(audio::MIC_CHUNK), audio::MIC_CHUNK_BYTES);
}

#[test]
fn every_turn_has_its_identifier() {
    assert_ne!(TurnId::new(), TurnId::new());
    let id = TurnId::new();
    assert_eq!(id.to_string().parse::<TurnId>().unwrap(), id);
}
