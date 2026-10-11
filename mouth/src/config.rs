//! The device configuration: one file per device, read by all the device programs.
//!
//! ```toml
//! # device = "home"                           # default: the hostname of the machine
//! bus = "nats://192.168.1.10:4222"
//! # bus_ca = "/etc/emory/ca.pem"              # turns TLS on with this authority
//! # bus_nkey_seed_file = "/etc/emory/home.nk" # the seed of the device's key (nkey)
//!
//! [mouth]
//! speaker = "default"                         # or the name of an output device
//! ```
//!
//! The sections of the other programs (`[ear]`, `[hands]`) are not ours: they are read as they
//! are and not looked at.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use contract::Device;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    device: Option<String>,
    /// The address of the NATS server.
    pub bus: String,
    /// The certificate authority of the bus (TLS); without it, the connection is in the clear.
    pub bus_ca: Option<PathBuf>,
    /// The file that holds the seed of the device's nkey.
    pub bus_nkey_seed_file: Option<PathBuf>,
    mouth: Option<Mouth>,
    // The other programs' sections: accepted, ignored.
    #[allow(dead_code)]
    ear: Option<toml::Value>,
    #[allow(dead_code)]
    hands: Option<toml::Value>,
}

/// The `[mouth]` section.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mouth {
    /// `"default"` or the name of an output device.
    #[serde(default = "default_speaker")]
    pub speaker: String,
}

fn default_speaker() -> String {
    "default".to_owned()
}

/// `~/.config/emory/config.toml`.
pub fn default_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set: give the configuration with --config")?;
    Ok(home.join("emory").join("config.toml"))
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        text.parse()
            .with_context(|| format!("configuration {}", path.display()))
    }

    /// The name of this device: the `device` key, else the hostname of the machine.
    pub fn device(&self) -> anyhow::Result<Device> {
        match &self.device {
            Some(name) => Device::new(name.as_str()).context("`device` in the configuration"),
            None => {
                let hostname = gethostname::gethostname().to_string_lossy().into_owned();
                Device::new(hostname.as_str()).with_context(|| {
                    format!(
                        "the hostname {hostname:?} cannot be a device name: \
                         set `device` in the configuration"
                    )
                })
            }
        }
    }

    /// The `[mouth]` section; this machine has no mouth without it.
    pub fn mouth(&self) -> anyhow::Result<&Mouth> {
        match &self.mouth {
            Some(mouth) => Ok(mouth),
            None => bail!(
                "there is no [mouth] section in the configuration: this device has no mouth. \
                 Add one (`[mouth]`, `speaker = \"default\"`) to use emory-mouth"
            ),
        }
    }
}

impl std::str::FromStr for Config {
    type Err = toml::de::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        toml::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_full_file() {
        let config: Config = r#"
            device = "home"
            bus = "nats://127.0.0.1:4222"
            bus_ca = "/etc/emory/ca.pem"
            bus_nkey_seed_file = "/etc/emory/home.nk"

            [ear]
            trigger = "shortcut"

            [mouth]
            speaker = "pipewire"

            [hands]
            modules = ["mpris"]
        "#
        .parse()
        .unwrap();
        assert_eq!(config.device().unwrap().as_str(), "home");
        assert_eq!(config.bus, "nats://127.0.0.1:4222");
        assert_eq!(config.mouth().unwrap().speaker, "pipewire");
        assert!(config.bus_ca.is_some() && config.bus_nkey_seed_file.is_some());
    }

    #[test]
    fn speaker_defaults_to_default() {
        let config: Config = "bus = \"nats://x:4222\"\n[mouth]\n".parse().unwrap();
        assert_eq!(config.mouth().unwrap().speaker, "default");
    }

    #[test]
    fn a_missing_mouth_section_is_a_clear_error() {
        let config: Config = "bus = \"nats://x:4222\"\n[ear]\n".parse().unwrap();
        let error = config.mouth().unwrap_err().to_string();
        assert!(error.contains("[mouth]"), "{error}");
    }

    #[test]
    fn a_bad_device_name_is_refused() {
        let config: Config = "device = \"Living Room\"\nbus = \"nats://x:4222\"\n"
            .parse()
            .unwrap();
        assert!(config.device().is_err());
    }

    #[test]
    fn the_hostname_is_the_default_device() {
        let config: Config = "bus = \"nats://x:4222\"".parse().unwrap();
        // Depends on the machine: either a valid name, or an error that points to the key.
        match config.device() {
            Ok(device) => assert!(!device.as_str().is_empty()),
            Err(error) => assert!(format!("{error:#}").contains("`device`")),
        }
    }

    #[test]
    fn an_unknown_mouth_key_is_refused() {
        let parsed: Result<Config, _> = "bus = \"x\"\n[mouth]\nspeeker = \"a\"".parse();
        assert!(parsed.is_err());
    }
}
