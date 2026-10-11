//! The device configuration: one file per device, `~/.config/emory/config.toml`, read by the
//! three device programs (`emory-ear`, `emory-mouth`, `emory-hands`). A program whose section is
//! missing does not run on this device. Each program reads its own section strictly and leaves
//! the others' alone.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use contract::Device;
use serde::{Deserialize, Deserializer};

const DEFAULT_BUS: &str = "nats://127.0.0.1:4222";

/// A path in the configuration file; `~/` stands for the home folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigPath(pub PathBuf);

impl<'de> Deserialize<'de> for ConfigPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self(expand(&raw, std::env::var_os("HOME"))))
    }
}

impl AsRef<Path> for ConfigPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

fn expand(raw: &str, home: Option<std::ffi::OsString>) -> PathBuf {
    match (raw.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(raw),
    }
}

/// The whole file. The `[mouth]` and `[hands]` sections belong to the other programs: they are
/// kept as they are and not looked into.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The name of the device; the hostname when absent.
    pub device: Option<String>,
    /// The URL of the NATS bus.
    #[serde(default = "default_bus")]
    pub bus: String,
    /// The certificate of the authority that signed the NATS server's (TLS is required if given).
    pub bus_ca: Option<ConfigPath>,
    /// The file with the seed of this device's nkey.
    pub bus_nkey_seed_file: Option<ConfigPath>,
    pub ear: Option<EarConfig>,
    #[allow(dead_code)]
    mouth: Option<toml::Table>,
    #[allow(dead_code)]
    hands: Option<toml::Table>,
}

fn default_bus() -> String {
    DEFAULT_BUS.to_owned()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EarConfig {
    /// `"default"` or the name of an input device.
    #[serde(default = "default_mic")]
    pub mic: String,
    #[serde(default)]
    pub trigger: TriggerKind,
    /// A press of the shortcut held at least this long is a hold, shorter is a tap.
    #[cfg_attr(not(feature = "shortcut"), allow(dead_code))]
    #[serde(default = "default_hold_threshold_ms")]
    pub hold_threshold_ms: u64,
}

fn default_mic() -> String {
    "default".to_owned()
}

fn default_hold_threshold_ms() -> u64 {
    300
}

/// What opens and closes a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// A keyboard shortcut, by the GlobalShortcuts portal.
    #[default]
    Shortcut,
    /// A physical button (to come).
    Button,
    /// Always listening (to come).
    Continuous,
}

impl TriggerKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Shortcut => "shortcut",
            Self::Button => "button",
            Self::Continuous => "continuous",
        }
    }
}

impl Config {
    /// Reads the file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the configuration {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in the configuration {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// The device: the `device` key, or else the hostname.
    pub fn device(&self) -> Result<Device> {
        match &self.device {
            Some(name) => Device::new(name).with_context(|| "the `device` key".to_owned()),
            None => {
                let host = hostname()?;
                Device::new(&host).with_context(|| {
                    format!("the hostname {host:?} cannot be a device name: set `device` in the configuration")
                })
            }
        }
    }
}

fn hostname() -> Result<String> {
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(name) = std::fs::read_to_string(path) {
            let name = name.trim();
            if !name.is_empty() {
                return Ok(name.to_owned());
            }
        }
    }
    bail!("cannot read the hostname: set `device` in the configuration")
}

/// `$XDG_CONFIG_HOME/emory/config.toml`, else `~/.config/emory/config.toml`.
pub fn default_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("emory/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_ear_section() {
        let config = Config::parse("device = \"home\"\n[ear]\n").unwrap();
        let ear = config.ear.unwrap();
        assert_eq!(config.bus, DEFAULT_BUS);
        assert_eq!(ear.mic, "default");
        assert_eq!(ear.trigger, TriggerKind::Shortcut);
        assert_eq!(ear.hold_threshold_ms, 300);
        assert_eq!(config.device.as_deref(), Some("home"));
    }

    #[test]
    fn missing_ear_section() {
        let config = Config::parse("[mouth]\nspeaker = \"default\"\n").unwrap();
        assert!(config.ear.is_none());
    }

    #[test]
    fn other_programs_sections_are_left_alone() {
        let text = "[ear]\n[mouth]\nanything = 1\n[hands.mpris]\nenabled = true\n";
        let config = Config::parse(text).unwrap();
        assert!(config.ear.is_some());
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(Config::parse("colour = 1\n").is_err());
        assert!(Config::parse("[ear]\nmicro = \"x\"\n").is_err());
        assert!(Config::parse("[ear]\ntrigger = \"telepathy\"\n").is_err());
    }

    #[test]
    fn the_example_file_is_valid() {
        let config = Config::parse(include_str!("../examples/config.toml")).unwrap();
        assert!(config.ear.is_some());
        assert!(config.mouth.is_some());
        assert!(config.hands.is_some());
        config.device().unwrap();
    }

    #[test]
    fn device_names_are_checked() {
        assert!(
            Config::parse("device = \"Home PC\"")
                .unwrap()
                .device()
                .is_err()
        );
        assert_eq!(
            Config::parse("device = \"home\"")
                .unwrap()
                .device()
                .unwrap()
                .as_str(),
            "home"
        );
    }

    #[test]
    fn tilde_is_expanded() {
        assert_eq!(
            expand("~/a/b", Some("/home/kris".into())),
            PathBuf::from("/home/kris/a/b")
        );
        assert_eq!(
            expand("/etc/x", Some("/home/kris".into())),
            PathBuf::from("/etc/x")
        );
    }
}
