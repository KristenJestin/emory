//! The bus subjects. The name of the device goes in as it is; a program that listens to all the
//! devices subscribes to the subject with `*` and finds the device with [`device_of_subject`].

use crate::{Device, device::valid_token};

/// The sentences transcribed while the owner is speaking ([`crate::Sentence`]).
pub const SENTENCE: &str = "hearing.sentence";
/// The whole turn, transcribed at the second press ([`crate::Turn`]).
pub const TURN: &str = "hearing.turn";
/// A sentence to say ([`crate::Say`]).
pub const SAY: &str = "voice.say";
/// Everything not yet said of a turn is thrown away ([`crate::Stop`]).
pub const STOP: &str = "voice.stop";
/// The mind asks all the ears, mouths and hands to announce themselves (empty message).
pub const ANNOUNCE_REQUEST: &str = "announce.request";

/// The presses of all the ears.
pub const ALL_BUTTONS: &str = "ear.*.button";
/// The audio of all the ears.
pub const ALL_EAR_AUDIO: &str = "ear.*.audio";
/// The announcements of all the ears.
pub const ALL_EAR_ANNOUNCES: &str = "ear.*.announce";
/// The announcements of all the mouths.
pub const ALL_MOUTH_ANNOUNCES: &str = "mouth.*.announce";
/// The announcements of all the hands.
pub const ALL_HANDS_ANNOUNCES: &str = "hands.*.announce";

/// The presses of an ear ([`crate::Button`]).
pub fn button(d: &Device) -> String {
    format!("ear.{d}.button")
}

/// The microphone audio of an ear (raw sound, see [`crate::audio`]).
pub fn ear_audio(d: &Device) -> String {
    format!("ear.{d}.audio")
}

/// The announcement of an ear ([`crate::Announce`]).
pub fn ear_announce(d: &Device) -> String {
    format!("ear.{d}.announce")
}

/// The voice of Emory, from the voice to the mouth (raw sound, see [`crate::audio`]).
pub fn mouth_audio(d: &Device) -> String {
    format!("mouth.{d}.audio")
}

/// The announcement of a mouth ([`crate::Announce`]).
pub fn mouth_announce(d: &Device) -> String {
    format!("mouth.{d}.announce")
}

/// The announcement of some hands ([`crate::Announce`]).
pub fn hands_announce(d: &Device) -> String {
    format!("hands.{d}.announce")
}

/// The voice starts or finishes speaking on a device ([`crate::Speaking`]).
pub fn speaking(d: &Device) -> String {
    format!("voice.{d}.speaking")
}

/// All the actions of some hands, to subscribe to them.
pub fn actions(d: &Device) -> String {
    format!("hands.{d}.>")
}

/// An action of some hands (`hands.office.mpris.pause`), a request with a reply
/// ([`crate::ActionReply`]). `None` if the name of the action cannot go into a subject.
pub fn action(d: &Device, name: &str) -> Option<String> {
    (name != "announce" && name.split('.').all(valid_token)).then(|| format!("hands.{d}.{name}"))
}

/// The device of a subject `ear.<name>.…`, `mouth.<name>.…`, `hands.<name>.…` or
/// `voice.<name>.speaking`. The other subjects of the voice (`voice.say`, `voice.stop`) have no
/// device: the device is in their message.
pub fn device_of_subject(subject: &str) -> Option<Device> {
    let parts: Vec<&str> = subject.split('.').collect();
    match parts[..] {
        ["ear" | "mouth" | "hands", name, ..] | ["voice", name, "speaking"] => {
            Device::new(name).ok()
        }
        _ => None,
    }
}

/// The name of the action of a subject `hands.<name>.<action>` (`mpris.pause`).
pub fn action_of_subject(subject: &str) -> Option<&str> {
    let rest = subject.strip_prefix("hands.")?;
    let (_, action) = rest.split_once('.')?;
    (action != "announce").then_some(action)
}
