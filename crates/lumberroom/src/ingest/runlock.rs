//! The run lock. Two runs on one machine serialise instead of interleaving their reads of the same
//! growing file.
//!
//! The monotonic advance in the repository is the correctness half and it holds across machines.
//! This is the second half: the owner is told to schedule a nightly Mode B run and Mode A is
//! interactive, so two runs overlapping is a normal Tuesday.

use std::io::{Read, Seek, SeekFrom, Write};

use fs2::FileExt;

use crate::client::{err, err_code, Result};
use crate::ingest::ingest_dir;

/// Ten seconds, from spec §6.2. A `plan` walk of this corpus takes longer than that, so the
/// second run is meant to give up and say what holds the lock rather than queue behind a walk.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const POLL: std::time::Duration = std::time::Duration::from_millis(200);

pub struct RunLock {
    /// Held for the life of the guard. The flock is on the open descriptor, so dropping this file
    /// is what releases it and an early close would hand the lock to a waiting run mid-walk.
    _file: std::fs::File,
}

impl RunLock {
    /// Rewrite the holder line. `plan` takes the lock before the server mints a run id, so the
    /// first holder string can only carry a pid, and the run id lands here a moment later.
    pub fn note(&self, holder: &str) -> Result<()> {
        write_holder(&self._file, holder)
    }
}

fn lock_path() -> Result<std::path::PathBuf> {
    let dir = ingest_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| err(format!("could not create {}: {e}", dir.display())))?;
    Ok(dir.join(".lock"))
}

fn write_holder(file: &std::fs::File, holder: &str) -> Result<()> {
    let mut handle = file;
    handle.set_len(0).map_err(|e| err(format!("could not clear the run lock: {e}")))?;
    handle
        .seek(SeekFrom::Start(0))
        .map_err(|e| err(format!("could not rewind the run lock: {e}")))?;
    handle
        .write_all(holder.as_bytes())
        .and_then(|_| handle.flush())
        .map_err(|e| err(format!("could not write the run lock: {e}")))
}

/// Whoever wrote the file last, for the error message. An empty or unreadable file gives a run
/// that predates the holder string, so say that instead of guessing.
fn current_holder(path: &std::path::Path) -> String {
    let mut text = String::new();
    match std::fs::File::open(path).and_then(|mut f| f.read_to_string(&mut text)) {
        Ok(_) if !text.trim().is_empty() => text.trim().to_string(),
        _ => "an unnamed run".to_string(),
    }
}

/// Take the exclusive lock, waiting up to ten seconds. A run that cannot take it exits 1 naming
/// what holds it.
pub fn acquire(holder: &str) -> Result<RunLock> {
    let path = lock_path()?;
    // No truncate on open: the holder string belongs to whoever won the lock, and clearing it
    // before winning leaves the timeout error with nobody to name.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| err(format!("could not open {}: {e}", path.display())))?;

    let deadline = std::time::Instant::now() + WAIT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                write_holder(&file, holder)?;
                return Ok(RunLock { _file: file });
            }
            Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(POLL),
            Err(e) => {
                let who = current_holder(&path);
                return Err(err_code(
                    format!(
                        "another ingest run holds {}: {who}. Waited {}s ({e}). \
                         Wait for it to finish, or delete the file if no run is alive.",
                        path.display(),
                        WAIT.as_secs()
                    ),
                    1,
                ));
            }
        }
    }
}

/// The holder string `plan` and `submit` write before they know a run id.
pub fn holder(command: &str) -> String {
    format!("{command} pid {} started {}", std::process::id(), chrono::Utc::now().to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lumberroom-runlock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn holder_round_trips_through_the_file() {
        let dir = scratch();
        let path = dir.join(".lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        write_holder(&file, "plan pid 42").unwrap();
        assert_eq!(current_holder(&path), "plan pid 42");

        // A shorter second holder must not leave the tail of the first behind.
        write_holder(&file, "run 9").unwrap();
        assert_eq!(current_holder(&path), "run 9");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_lock_file_names_nobody() {
        let dir = scratch();
        let path = dir.join(".lock");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(current_holder(&path), "an unnamed run");
        assert_eq!(current_holder(&dir.join("absent")), "an unnamed run");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn holder_carries_the_pid() {
        let h = holder("plan");
        assert!(h.starts_with("plan pid "));
        assert!(h.contains(&std::process::id().to_string()));
    }
}
