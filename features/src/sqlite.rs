//! The engine's read-only SQLite files: the A2AJ store and the files beside it, the note-up graph, the journals
//! database, CourtListener's bulk store. Each is opened once and its connection kept while its copy of the file is:
//! opening and closing one costs more than most reads, and an installer that writes a store in place does so through
//! its write-ahead log, which readers may hold open.

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

/// Which copy of a file a connection, or what was read through it, came from: a file replaced in its place is another
/// copy.
pub type Stamp = (u64, Option<SystemTime>);

static CONNECTIONS: Mutex<Vec<(PathBuf, Stamp, Connection)>> = Mutex::new(Vec::new());

fn connections() -> MutexGuard<'static, Vec<(PathBuf, Stamp, Connection)>> {
    CONNECTIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Closes every kept connection, so the files can be replaced or removed; the next read opens its file again.
pub fn forget() {
    connections().clear();
}

/// A SQLite failure as the engine reports one.
pub fn sql(error: rusqlite::Error) -> String {
    error.to_string()
}

/// Runs `work` on a read-only connection to the file at `path`, given the copy it reads; None where there is no file.
/// A document's text runs over many pages, so the file is read through a mapping of it, not a call a page.
pub fn read<T>(path: &Path, work: impl FnOnce(&Connection, Stamp) -> Result<T, String>) -> Result<Option<T>, String> {
    kept(path, true, work)
}

/// [`read`] for a file its builder replaces whole (the note-up graph): unmapped, as a mapping holds the file on Windows.
pub fn read_unmapped<T>(path: &Path, work: impl FnOnce(&Connection, Stamp) -> Result<T, String>) -> Result<Option<T>, String> {
    kept(path, false, work)
}

fn kept<T>(path: &Path, mapped: bool, work: impl FnOnce(&Connection, Stamp) -> Result<T, String>) -> Result<Option<T>, String> {
    let Some(stamp) = std::fs::metadata(path).ok().map(|metadata| (metadata.len(), metadata.modified().ok())) else { return Ok(None) };
    let kept = {
        let mut connections = connections();
        connections.retain(|(other, held, _)| other != path || *held == stamp);
        connections.iter().position(|(other, _, _)| other == path).map(|index| connections.swap_remove(index).2)
    };
    let connection = match kept {
        Some(connection) => connection,
        None => {
            let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .map_err(sql)?;
            if mapped {
                connection.execute_batch("PRAGMA mmap_size=2147418112").map_err(sql)?;
            }
            connection
        }
    };
    let result = work(&connection, stamp);
    connections().push((path.to_owned(), stamp, connection));
    result.map(Some)
}
