//! The file of the timers and reminders: one JSON file in the data folder, written whole at each
//! change (to a file beside it, then renamed over: never half written) and read again when `mind`
//! starts. A file that cannot be read is put aside and the module starts without timers.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use contract::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

/// The version of the format this code writes and reads.
const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A duration: at its end, « le minuteur est fini ».
    Timer,
    /// A text at a time.
    Reminder,
}

/// A timer or a reminder. The due time is an absolute instant, with the offset it had when it was
/// set: a restart changes nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// A small number, never used twice (the LLM names the item by it).
    pub id: u64,
    pub kind: Kind,
    /// A timer: what it is for, if the person said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A reminder: what to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A timer: its length, which a phrase without label says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    pub created: Timestamp,
    pub due: Timestamp,
    /// The device of the conversation that set it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// What the module gives to [`Store::add`]; the store draws the id.
pub struct NewItem {
    pub kind: Kind,
    pub label: Option<String>,
    pub text: Option<String>,
    pub seconds: Option<u64>,
    pub created: Timestamp,
    pub due: Timestamp,
    pub device: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    next_id: u64,
    items: Vec<Item>,
}

/// The items, and where they are kept.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    items: Vec<Item>,
    next_id: u64,
}

/// A store, as found at start-up.
#[derive(Debug)]
pub struct Opened {
    pub store: Store,
    /// Where the file went when it could not be read.
    pub broken: Option<PathBuf>,
}

impl Store {
    /// Reads the file, creating its folder. A missing file is a new store; a file that cannot be
    /// read (unreadable, not JSON, a newer version, items that contradict themselves) is renamed
    /// `<file>.broken-<date>` and the store starts empty, with an `error` in the logs. Only a
    /// folder that cannot be made fails.
    pub fn open(path: &Path, now: DateTime<Utc>) -> io::Result<Opened> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let mut store = Self {
            path: path.to_owned(),
            items: Vec::new(),
            next_id: 1,
        };
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Opened {
                    store,
                    broken: None,
                });
            }
            Err(e) => return Ok(store.put_aside(now, &format!("unreadable: {}", e.kind()))),
        };
        match parse(&text) {
            Ok(document) => {
                store.next_id = document.next_id;
                store.items = document.items;
                Ok(Opened {
                    store,
                    broken: None,
                })
            }
            Err(why) => Ok(store.put_aside(now, &why)),
        }
    }

    /// Renames the file that cannot be read, and gives the empty store. The content stays out of
    /// the logs: it holds what the person asked to be reminded of.
    fn put_aside(self, now: DateTime<Utc>, why: &str) -> Opened {
        let stamp = now.format("%Y%m%dT%H%M%SZ");
        let name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let mut aside = self.path.with_file_name(format!("{name}.broken-{stamp}"));
        for n in 2.. {
            if !aside.exists() {
                break;
            }
            aside = self
                .path
                .with_file_name(format!("{name}.broken-{stamp}-{n}"));
        }
        let broken = match fs::rename(&self.path, &aside) {
            Ok(()) => {
                error!(file = %self.path.display(), aside = %aside.display(), why, "the file of the timers cannot be read: put aside, starting without timers");
                Some(aside)
            }
            Err(e) => {
                error!(file = %self.path.display(), why, rename = ?e.kind(), "the file of the timers cannot be read, and cannot be put aside: starting without timers");
                None
            }
        };
        Opened {
            store: self,
            broken,
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keeps a new item. On failure to write, nothing changed.
    pub fn add(&mut self, new: NewItem) -> io::Result<Item> {
        let item = Item {
            id: self.next_id,
            kind: new.kind,
            label: new.label,
            text: new.text,
            seconds: new.seconds,
            created: new.created,
            due: new.due,
            device: new.device,
        };
        let mut items = self.items.clone();
        items.push(item.clone());
        self.write(&items, self.next_id + 1)?;
        self.items = items;
        self.next_id += 1;
        Ok(item)
    }

    /// Takes an item away: `None` if there is none of this id. On failure to write, nothing
    /// changed.
    pub fn remove(&mut self, id: u64) -> io::Result<Option<Item>> {
        let Some(at) = self.items.iter().position(|i| i.id == id) else {
            return Ok(None);
        };
        let mut items = self.items.clone();
        let item = items.remove(at);
        self.write(&items, self.next_id)?;
        self.items = items;
        Ok(Some(item))
    }

    /// Drops what came due more than `limit` ago (what a stop of `mind` longer than that missed:
    /// it is no longer worth saying) and returns it, for the logs.
    pub fn discard_late(&mut self, now: DateTime<Utc>, limit: Duration) -> io::Result<Vec<Item>> {
        let (late, kept): (Vec<Item>, Vec<Item>) = self
            .items
            .iter()
            .cloned()
            .partition(|i| now - i.due.with_timezone(&Utc) > limit);
        if late.is_empty() {
            return Ok(late);
        }
        self.write(&kept, self.next_id)?;
        self.items = kept;
        Ok(late)
    }

    /// Writes beside, flushes, renames over: a reader sees the old file or the new one.
    fn write(&self, items: &[Item], next_id: u64) -> io::Result<()> {
        let document = serde_json::to_vec_pretty(&serde_json::json!({
            "version": VERSION,
            "next_id": next_id,
            "items": items,
        }))
        .map_err(io::Error::other)?;
        let name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let beside = self.path.with_file_name(format!("{name}.tmp"));
        let mut file = open_private(&beside)?;
        file.write_all(&document)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&beside, &self.path)?;
        // The rename itself must survive a power cut.
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            File::open(dir)?.sync_all()?;
        }
        info!(items = items.len(), "timers written");
        Ok(())
    }
}

/// The file, readable by the owner alone: a reminder can be personal.
fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Why a text is not a store this code can use, without quoting it.
fn parse(text: &str) -> Result<Document, String> {
    // Only the category of the error: its text could quote the content.
    let document: Document = serde_json::from_str(text)
        .map_err(|e| format!("not a store ({:?} error)", e.classify()))?;
    if document.version != VERSION {
        return Err(format!("version {} is not {VERSION}", document.version));
    }
    let mut seen = std::collections::HashSet::new();
    for item in &document.items {
        if !seen.insert(item.id) || item.id >= document.next_id {
            return Err("ids that repeat or reach next_id".to_owned());
        }
        let complete = match item.kind {
            Kind::Timer => item.seconds.is_some(),
            Kind::Reminder => item.text.is_some(),
        };
        if !complete {
            return Err("an item lacks what its kind needs".to_owned());
        }
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-13T17:00:00Z".parse().unwrap()
    }

    fn timer(label: &str, seconds: u64) -> NewItem {
        let created: Timestamp = "2026-10-13T19:00:00+02:00".parse().unwrap();
        NewItem {
            kind: Kind::Timer,
            label: Some(label.to_owned()),
            text: None,
            seconds: Some(seconds),
            created,
            due: created + Duration::seconds(seconds as i64),
            device: Some("office".to_owned()),
        }
    }

    fn open(dir: &Path) -> Opened {
        Store::open(&dir.join("timers.json"), now()).unwrap()
    }

    #[test]
    fn a_new_folder_and_no_file_give_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/er/timers.json");
        let opened = Store::open(&path, now()).unwrap();
        assert!(opened.store.items().is_empty() && opened.broken.is_none());
        assert!(path.parent().unwrap().is_dir());
    }

    #[test]
    fn what_is_written_is_read_again_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path()).store;
        let first = store.add(timer("pâtes", 600)).unwrap();
        let second = store.add(timer("thé", 180)).unwrap();
        assert_eq!((first.id, second.id), (1, 2));
        drop(store);
        let opened = open(dir.path());
        assert!(opened.broken.is_none());
        assert_eq!(opened.store.items(), [first, second]);
    }

    #[test]
    fn an_id_is_never_used_twice_even_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path()).store;
        store.add(timer("a", 60)).unwrap();
        let b = store.add(timer("b", 60)).unwrap();
        store.remove(b.id).unwrap();
        drop(store);
        let mut store = open(dir.path()).store;
        assert_eq!(store.add(timer("c", 60)).unwrap().id, 3);
    }

    #[test]
    fn removing_gives_the_item_back_and_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path()).store;
        let item = store.add(timer("pâtes", 600)).unwrap();
        assert_eq!(store.remove(9).unwrap(), None);
        assert_eq!(store.remove(item.id).unwrap(), Some(item));
        assert!(store.items().is_empty());
        assert!(open(dir.path()).store.items().is_empty());
    }

    #[test]
    fn the_format_is_the_one_the_spec_shows() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path()).store;
        store.add(timer("pâtes", 600)).unwrap();
        let text = fs::read_to_string(dir.path().join("timers.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "version": 1, "next_id": 2,
                "items": [{
                    "id": 1, "kind": "timer", "label": "pâtes", "seconds": 600,
                    "created": "2026-10-13T19:00:00+02:00",
                    "due": "2026-10-13T19:10:00+02:00", "device": "office"
                }]
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_the_owners_alone_and_nothing_is_left_beside_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path()).store;
        store.add(timer("pâtes", 600)).unwrap();
        let mode = fs::metadata(dir.path().join("timers.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["timers.json"]);
    }

    #[test]
    fn a_failed_write_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path()).store;
        let kept = store.add(timer("a", 60)).unwrap();
        // The folder disappears under the store.
        let gone = dir.path().join("gone");
        fs::create_dir(&gone).unwrap();
        let mut orphan = Store::open(&gone.join("timers.json"), now()).unwrap().store;
        fs::remove_dir_all(&gone).unwrap();
        assert!(orphan.add(timer("b", 60)).is_err());
        assert!(orphan.items().is_empty());
        assert_eq!(store.items(), [kept]);
    }

    #[test]
    fn a_broken_file_is_put_aside_and_the_store_starts_empty() {
        for (case, content) in [
            ("not json", "ceci n'est pas du json".to_owned()),
            ("empty", String::new()),
            ("truncated", "{\"version\":1,\"next_id\":2,\"items\":[{\"id\"".to_owned()),
            ("newer version", "{\"version\":2,\"next_id\":1,\"items\":[]}".to_owned()),
            (
                "an id that repeats",
                r#"{"version":1,"next_id":3,"items":[
                    {"id":1,"kind":"timer","seconds":60,"created":"2026-10-13T19:00:00+02:00","due":"2026-10-13T19:01:00+02:00"},
                    {"id":1,"kind":"timer","seconds":60,"created":"2026-10-13T19:00:00+02:00","due":"2026-10-13T19:01:00+02:00"}]}"#.to_owned(),
            ),
            (
                "a reminder without text",
                r#"{"version":1,"next_id":2,"items":[
                    {"id":1,"kind":"reminder","created":"2026-10-13T19:00:00+02:00","due":"2026-10-13T19:01:00+02:00"}]}"#.to_owned(),
            ),
            (
                "an id past next_id",
                r#"{"version":1,"next_id":1,"items":[
                    {"id":1,"kind":"timer","seconds":60,"created":"2026-10-13T19:00:00+02:00","due":"2026-10-13T19:01:00+02:00"}]}"#.to_owned(),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("timers.json"), &content).unwrap();
            let opened = open(dir.path());
            assert!(opened.store.items().is_empty(), "{case}");
            let aside = opened.broken.unwrap_or_else(|| panic!("{case}: not put aside"));
            assert_eq!(
                aside.file_name().unwrap(),
                "timers.json.broken-20261013T170000Z",
                "{case}"
            );
            // Nothing is lost: the file is there to repair by hand.
            assert_eq!(fs::read_to_string(&aside).unwrap(), content, "{case}");
            assert!(!dir.path().join("timers.json").exists(), "{case}");
            // And the store works again.
            let mut store = opened.store;
            assert_eq!(store.add(timer("a", 60)).unwrap().id, 1, "{case}");
        }
    }

    #[test]
    fn two_broken_files_the_same_second_keep_both() {
        let dir = tempfile::tempdir().unwrap();
        for content in ["un", "deux"] {
            fs::write(dir.path().join("timers.json"), content).unwrap();
            assert!(open(dir.path()).broken.is_some());
        }
        let mut asides: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        asides.sort();
        assert_eq!(
            asides,
            [
                "timers.json.broken-20261013T170000Z",
                "timers.json.broken-20261013T170000Z-2"
            ]
        );
    }

    #[test]
    fn what_came_due_long_ago_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path()).store;
        // Due 19:10 +02:00 = 17:10Z.
        let old = store.add(timer("pâtes", 600)).unwrap();
        let mut soon = timer("thé", 600);
        soon.due += Duration::hours(14);
        let soon = store.add(soon).unwrap();
        let limit = Duration::hours(12);
        // 11 hours late: kept.
        assert!(
            store
                .discard_late(now() + Duration::hours(11), limit)
                .unwrap()
                .is_empty()
        );
        // 13 hours late: discarded, and gone from the file.
        let late = store
            .discard_late(now() + Duration::hours(13), limit)
            .unwrap();
        assert_eq!(late, [old]);
        assert_eq!(store.items(), std::slice::from_ref(&soon));
        assert_eq!(open(dir.path()).store.items(), [soon]);
    }
}
