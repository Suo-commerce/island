//! A session's events, on disk: one JSON object per line, appended and
//! synced one event at a time. The file only grows; a line cut short by
//! a crash was never an event and is dropped on opening.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flare_core::session::{Event, EventKind};

/// One session's events.
pub struct Store {
    path: PathBuf,
    file: File,
    events: Vec<Event>,
}

impl Store {
    /// Open the session file at `path`, creating it if there is none.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;

        // Every complete line is an event, in order. Only the end may be
        // unfinished: a line without its newline.
        let complete = text.rfind('\n').map_or(0, |end| end + 1);
        let mut events = Vec::new();
        for (index, line) in text[..complete].lines().enumerate() {
            let event: Event = serde_json::from_str(line).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: line {}: {error}", path.display(), index + 1),
                )
            })?;
            if event.seq != index as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{}: line {} holds event {}",
                        path.display(),
                        index + 1,
                        event.seq
                    ),
                ));
            }
            events.push(event);
        }
        if complete < text.len() {
            file.set_len(complete as u64)?;
            file.sync_all()?;
        }
        Ok(Self {
            path: path.into(),
            file,
            events,
        })
    }

    /// Where the events are.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every event, in order.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The place the next event takes.
    pub fn next(&self) -> u64 {
        self.events.len() as u64
    }

    /// Record an event. It is durable when this returns.
    pub fn append(
        &mut self,
        at: u64,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> io::Result<Event> {
        let event = Event {
            seq: self.next(),
            // Time never goes back within a session.
            at: self.events.last().map_or(at, |last| at.max(last.at)),
            kind,
            payload,
        };
        let mut line = serde_json::to_string(&event).map_err(io::Error::other)?;
        line.push('\n');
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()?;
        self.events.push(event.clone());
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use flare_core::session::EventKind;

    use super::Store;

    #[test]
    fn events_are_kept_in_order_and_a_cut_line_is_dropped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sessions").join("main.jsonl");
        let mut store = Store::open(&path).expect("opens");
        for n in 0..3u64 {
            let event = store
                .append(
                    100 - n,
                    EventKind::Error,
                    serde_json::json!({"text": n.to_string()}),
                )
                .expect("appended");
            assert_eq!(event.seq, n);
            // Time does not go back within a session.
            assert_eq!(event.at, 100);
        }
        drop(store);

        let mut text = std::fs::read_to_string(&path).expect("read");
        text.push_str("{\"seq\":3,\"at\":");
        std::fs::write(&path, &text).expect("write");
        let store = Store::open(&path).expect("opens");
        assert_eq!(store.next(), 3);
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .ends_with("}\n")
        );
        drop(store);

        // A complete line that is not an event is damage, not a cut.
        std::fs::write(&path, "{\"seq\":0}\nnot json\n").expect("write");
        assert!(Store::open(&path).is_err());
    }
}
