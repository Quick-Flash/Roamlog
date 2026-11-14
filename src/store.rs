use crate::model::{Event, State, Status};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// Journal shared by Android UI and recorder processes. Writers hold exclusive file lock
/// and first apply lines other process appended, so validation sees current state.
pub struct Store {
    pub state: State,
    path: PathBuf,
    journal: File,
    /// Journal bytes applied to `state`.
    applied: u64,
    failed: bool,
}

impl Store {
    pub fn revision(&self) -> u64 {
        self.applied
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let mut state = State::default();
        let mut offset = 0;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        // Exclusive: trimming partial final line must not race other process's write.
        crate::flock::lock(&file)?;
        let mut reader = BufReader::new(&file);
        loop {
            let mut line = Vec::new();
            let n = reader
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if line.last() != Some(&b'\n') {
                // Power loss can leave a partial final write. Never discard committed lines.
                file.set_len(offset).map_err(|e| e.to_string())?;
                file.sync_data().map_err(|e| e.to_string())?;
                break;
            }
            let event: Event = serde_json::from_slice(&line)
                .map_err(|e| format!("Invalid journal at byte {offset}: {e}"))?;
            state.validate(&event)?;
            state.apply(event);
            offset += n as u64;
        }
        crate::flock::unlock(&file);
        Ok(Self {
            state,
            path: path.to_owned(),
            journal: file,
            applied: offset,
            failed: false,
        })
    }

    /// Restore renames a new journal into place; reopen it instead of old file.
    fn reopen_if_replaced(&mut self) -> Result<(), String> {
        let current = std::fs::metadata(&self.path).map_err(|e| e.to_string())?;
        let open = self.journal.metadata().map_err(|e| e.to_string())?;
        if (current.dev(), current.ino()) != (open.dev(), open.ino()) {
            *self = Self::open(&self.path)?;
        }
        Ok(())
    }

    /// Committed lines only; a write in progress may leave a partial final line.
    pub fn backup(&mut self) -> Result<Vec<u8>, String> {
        self.reopen_if_replaced()?;
        let mut bytes = Vec::new();
        File::open(&self.path)
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(|e| e.to_string())?;
        bytes.truncate(bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1));
        Ok(bytes)
    }

    /// Applies complete lines appended by other process since last read.
    pub fn catch_up(&mut self) -> Result<(), String> {
        self.reopen_if_replaced()?;
        let len = self.journal.metadata().map_err(|e| e.to_string())?.len();
        if len == self.applied {
            return Ok(());
        }
        let mut file = &self.journal;
        file.seek(SeekFrom::Start(self.applied))
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(file);
        loop {
            let mut line = Vec::new();
            let n = reader
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if n == 0 || line.last() != Some(&b'\n') {
                return Ok(());
            }
            let event: Event = serde_json::from_slice(&line)
                .map_err(|e| format!("Invalid journal at byte {}: {e}", self.applied))?;
            self.state.validate(&event)?;
            self.state.apply(event);
            self.applied += n as u64;
        }
    }

    pub fn commit(&mut self, event: Event) -> Result<(), String> {
        if self.failed {
            return Err("Storage failed; reopen before recording".into());
        }
        self.reopen_if_replaced()?;
        crate::flock::lock(&self.journal)?;
        let result = self.commit_locked(event);
        crate::flock::unlock(&self.journal);
        result
    }

    fn commit_locked(&mut self, event: Event) -> Result<(), String> {
        self.catch_up()?;
        self.state.validate(&event)?;
        let mut bytes = serde_json::to_vec(&event).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        if let Err(e) = self
            .journal
            .write_all(&bytes)
            .and_then(|_| self.journal.sync_data())
        {
            self.failed = true;
            return Err(format!("Could not save trip: {e}"));
        }
        self.applied += bytes.len() as u64;
        self.state.apply(event);
        Ok(())
    }

    /// Process death is a path break. Pause at last observation, not time app reopens.
    pub fn recover_recording(&mut self) -> Result<(), String> {
        if let Some(trip) = self
            .state
            .active()
            .filter(|t| t.status == Status::Recording)
        {
            let timestamp_ms = trip
                .latest_fix_ms
                .unwrap_or(trip.recording_since_ms.unwrap());
            self.commit(Event::Pause { timestamp_ms })?;
        }
        Ok(())
    }
}
