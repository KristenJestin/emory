//! The `[data]` and `[time]` sections of the configuration (`specs/time.md`). Every key has a
//! default: a missing section gives the module with these values.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::ConfigPath;

use super::zone::Zone;

/// Where `mind` keeps what must outlive it (`[data]`): the timers today, more later.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DataConfig {
    /// The folder, created if it is missing. Default: `$XDG_STATE_HOME/emory/mind`, else
    /// `~/.local/state/emory/mind`. In a container, a volume.
    pub dir: Option<ConfigPath>,
}

impl DataConfig {
    /// The folder of the data, with the default applied.
    pub fn dir(&self) -> Result<PathBuf> {
        if let Some(dir) = &self.dir {
            return Ok(dir.0.clone());
        }
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .context("data.dir is not set, and neither XDG_STATE_HOME nor HOME gives a default")?;
        Ok(state.join("emory/mind"))
    }

    pub fn check(&self) -> Result<()> {
        if let Some(dir) = &self.dir {
            anyhow::ensure!(
                !dir.0.as_os_str().is_empty(),
                "data.dir must not be empty (leave it out for the default)"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TimeConfig {
    /// `false` takes the four tools away (the file is left as it is).
    pub enabled: bool,
    /// The file of the timers. Default: `timers.json` in `data.dir`.
    pub store: Option<ConfigPath>,
    /// The zone in which a reminder « at nine » is understood and the hours are said: an IANA
    /// name (`Europe/Paris`); empty, the system's (`TZ`).
    pub timezone: String,
    /// The device that rings when the timer has none; without it, where the owner last spoke.
    pub default_device: Option<String>,
    /// The most timers and reminders waiting at once.
    pub max_active: usize,
    /// A short tone before the phrase.
    pub chime: bool,
    /// How many times the phrase is said again if nobody pressed.
    pub repeat: u32,
    /// Seconds before it is said again.
    pub repeat_after_s: u64,
    /// What came due during a stop longer than this many hours is dropped, not said; what is less
    /// late is said « Avec du retard ».
    pub late_limit_hours: u64,
    /// What is said when a labelled timer ends. `{label}`.
    pub timer_done: String,
    /// What is said when a timer without a label ends. `{duration}`, in words.
    pub timer_done_unlabeled: String,
    /// What is said when a reminder comes due. `{text}`.
    pub reminder: String,
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            store: None,
            timezone: String::new(),
            default_device: None,
            max_active: 50,
            chime: true,
            repeat: 1,
            repeat_after_s: 60,
            late_limit_hours: 12,
            timer_done: "Le minuteur {label} est fini.".to_owned(),
            timer_done_unlabeled: "Le minuteur de {duration} est fini.".to_owned(),
            reminder: "Rappel : {text}.".to_owned(),
        }
    }
}

impl TimeConfig {
    /// The file of the timers, in the data folder unless `store` says otherwise.
    pub fn store_path(&self, data: &DataConfig) -> Result<PathBuf> {
        match &self.store {
            Some(path) => Ok(path.0.clone()),
            None => Ok(data.dir()?.join("timers.json")),
        }
    }

    /// Refuses absurd values, naming the key.
    pub fn check(&self) -> Result<()> {
        Zone::new(&self.timezone).context("time.timezone")?;
        if let Some(store) = &self.store {
            anyhow::ensure!(
                store.0.file_name().is_some(),
                "time.store must be the path of a file"
            );
        }
        if let Some(device) = &self.default_device {
            anyhow::ensure!(
                contract::Device::new(device.as_str()).is_ok(),
                "time.default_device must be a device name (lowercase letters, digits and dashes)"
            );
        }
        anyhow::ensure!(
            (1..=500).contains(&self.max_active),
            "time.max_active must be between 1 and 500"
        );
        anyhow::ensure!(self.repeat <= 5, "time.repeat must be between 0 and 5");
        anyhow::ensure!(
            (10..=600).contains(&self.repeat_after_s),
            "time.repeat_after_s must be between 10 and 600"
        );
        anyhow::ensure!(
            (1..=168).contains(&self.late_limit_hours),
            "time.late_limit_hours must be between 1 and 168"
        );
        template("time.timer_done", &self.timer_done, "label")?;
        template(
            "time.timer_done_unlabeled",
            &self.timer_done_unlabeled,
            "duration",
        )?;
        template("time.reminder", &self.reminder, "text")?;
        Ok(())
    }
}

/// A phrase with its one placeholder: present, and the only one.
fn template(key: &str, text: &str, placeholder: &str) -> Result<()> {
    anyhow::ensure!(
        text.chars().any(char::is_alphanumeric),
        "{key} must be a sentence to say"
    );
    anyhow::ensure!(
        text.contains(&format!("{{{placeholder}}}")),
        "{key} must contain {{{placeholder}}}"
    );
    let others = text
        .replace(&format!("{{{placeholder}}}"), "")
        .contains(['{', '}']);
    anyhow::ensure!(
        !others,
        "{key} may only use {{{placeholder}}} between braces"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<TimeConfig> {
        let config: TimeConfig = toml::from_str(toml)?;
        config.check()?;
        Ok(config)
    }

    #[test]
    fn an_empty_section_gives_the_defaults() {
        let c = parse("").unwrap();
        assert!(c.enabled && c.chime && c.timezone.is_empty() && c.store.is_none());
        assert_eq!((c.max_active, c.repeat, c.repeat_after_s), (50, 1, 60));
        assert_eq!(c.late_limit_hours, 12);
        assert_eq!(c.reminder, "Rappel : {text}.");
        assert!(TimeConfig::default().check().is_ok());
    }

    #[test]
    fn the_keys_are_read() {
        let c = parse(
            "enabled = false\nstore = \"/srv/t.json\"\ntimezone = \"Europe/Paris\"\n\
             default_device = \"office\"\nmax_active = 5\nchime = false\nrepeat = 0\n\
             repeat_after_s = 30\nlate_limit_hours = 24\ntimer_done = \"{label} : fini.\"\n\
             timer_done_unlabeled = \"{duration}, fini.\"\nreminder = \"{text} !\"\n",
        )
        .unwrap();
        assert!(!c.enabled && !c.chime);
        assert_eq!(c.timezone, "Europe/Paris");
        assert_eq!(c.default_device.as_deref(), Some("office"));
        assert_eq!((c.max_active, c.repeat, c.repeat_after_s), (5, 0, 30));
        assert_eq!(c.late_limit_hours, 24);
        assert_eq!(
            c.store_path(&DataConfig::default()).unwrap(),
            PathBuf::from("/srv/t.json")
        );
    }

    #[test]
    fn the_store_is_in_the_data_folder_by_default() {
        let data = DataConfig {
            dir: Some(ConfigPath(PathBuf::from("/var/lib/emory"))),
        };
        assert_eq!(
            TimeConfig::default().store_path(&data).unwrap(),
            PathBuf::from("/var/lib/emory/timers.json")
        );
        // Without `dir`, a state folder of the user's.
        let default = DataConfig::default().dir().unwrap();
        assert!(default.ends_with("emory/mind"), "{default:?}");
    }

    #[test]
    fn a_wrong_value_is_refused_with_the_name_of_its_key() {
        for (bad, key) in [
            ("timezone = \"Mars/Olympus\"", "time.timezone"),
            ("store = \"/\"", "time.store"),
            ("default_device = \"Office\"", "time.default_device"),
            ("default_device = \"\"", "time.default_device"),
            ("max_active = 0", "time.max_active"),
            ("max_active = 5000", "time.max_active"),
            ("repeat = 6", "time.repeat"),
            ("repeat_after_s = 5", "time.repeat_after_s"),
            ("repeat_after_s = 601", "time.repeat_after_s"),
            ("late_limit_hours = 0", "time.late_limit_hours"),
            ("late_limit_hours = 500", "time.late_limit_hours"),
            ("timer_done = \"Fini.\"", "time.timer_done"),
            ("timer_done = \"{duration} fini.\"", "time.timer_done"),
            ("timer_done = \"{label} {x} fini.\"", "time.timer_done"),
            (
                "timer_done_unlabeled = \"Fini.\"",
                "time.timer_done_unlabeled",
            ),
            ("reminder = \"Rappel.\"", "time.reminder"),
            ("reminder = \"{text} }\"", "time.reminder"),
        ] {
            let error = format!("{:#}", parse(bad).unwrap_err());
            assert!(error.contains(key), "{bad}: {error}");
        }
        // An unknown key, a value of the wrong type.
        for bad in ["volume = 3", "repeat = \"once\"", "enabled = 1"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_data_section_is_a_folder_or_nothing() {
        let data: DataConfig = toml::from_str("dir = \"~/state\"").unwrap();
        assert!(data.check().is_ok());
        assert!(data.dir().unwrap().ends_with("state"));
        assert!(toml::from_str::<DataConfig>("folder = \"x\"").is_err());
        let empty: DataConfig = toml::from_str("dir = \"\"").unwrap();
        assert!(empty.check().is_err());
    }
}
