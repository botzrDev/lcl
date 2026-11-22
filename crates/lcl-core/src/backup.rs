//! Copying a database file while it is in use: `lcl backup`, and the
//! snapshot taken before an upgrade.
//!
//! The copy is made with SQLite's online backup API, page for page, through
//! the open connection. It is never made with `VACUUM INTO`: the search
//! indexes name their rows by rowid, and a vacuum may renumber rowids.
//!
//! # States
//!
//! A copy to `DEST` moves through these, in order:
//!
//! 1. **Absent.** Nothing is at `DEST`. If something is, the copy is refused
//!    here and nothing is touched.
//! 2. **Filling.** A temporary file in `DEST`'s directory, named
//!    `.NAME.lcl-backup-PID-RANDOM.tmp`, receives the pages.
//! 3. **Checked.** The temporary file holds every page, `PRAGMA
//!    integrity_check` on it says `ok`, its connection is closed and its
//!    bytes are synced.
//! 4. **Placed.** `DEST` is that file, and the temporary name is gone.
//!
//! An error in 2 or 3 removes the temporary file and reports the path. A
//! process killed in 2 or 3 leaves the temporary file behind (nothing can
//! remove it), and still nothing at `DEST`.
//!
//! # Invariants
//!
//! - **Whole or absent.** `DEST` either does not exist or is a complete
//!   database. No reader ever finds part of one there, also not after a
//!   kill, a full disk or an unwritable directory.
//! - **Never replaced.** A file that exists at `DEST`, of any kind, is left
//!   byte for byte as it was. Placing is a hard link, which fails if `DEST`
//!   has appeared since state 1.
//! - **One committed state.** The copy is the source as one transaction
//!   boundary left it: every row of a committed write, no row of one that
//!   was open or came later. The pages are read in one read transaction;
//!   in WAL mode a writer is not blocked by it and does not disturb it.
//! - **Same rows, same rowids.** Pages are copied as they are, so the copy
//!   has the source's `user_version`, and its search indexes match its rows
//!   exactly as the source's do.
//! - **Source untouched.** The copy only reads the source.
//! - **No litter on success or failure.** When the call returns, `DEST`'s
//!   directory holds one more file than before (success) or none (failure).

use crate::db::BUSY_TIMEOUT;
use crate::error::{LclError, Result};
use rusqlite::Connection;
use rusqlite::backup::{Backup, StepResult};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What a finished copy is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BackupReport {
    /// The copy's `PRAGMA user_version`, read from the copy itself.
    pub schema_version: i64,
    /// The copy's size.
    pub bytes: u64,
}

/// Copies the database `src` has open to a new file at `dest`. See the
/// module documentation for what holds at every moment.
pub(crate) fn copy_database(src: &Connection, dest: &Path) -> Result<BackupReport> {
    copy_database_with(src, dest, |_| Ok(()))
}

/// `copy_database`, with `prepare` run on the temporary file's connection
/// before any page is copied. A test uses it to make the copy fail part-way.
fn copy_database_with(
    src: &Connection,
    dest: &Path,
    prepare: impl FnOnce(&Connection) -> rusqlite::Result<()>,
) -> Result<BackupReport> {
    let shown = dest.display().to_string();
    // State 1. `symlink_metadata`, so a dangling link counts as existing.
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(LclError::BackupExists(shown));
    }
    let failed = |reason: String| LclError::BackupFailed {
        path: shown.clone(),
        reason,
    };
    let dir = match dest.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = dest
        .file_name()
        .ok_or_else(|| failed("it names no file".into()))?
        .to_string_lossy()
        .into_owned();
    let tmp = dir.join(format!(
        ".{name}.lcl-backup-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));

    let written = fill(src, &tmp, prepare)
        .map_err(|e| failed(e.to_string()))
        .and_then(|report| place(&tmp, dest, &shown).map(|()| report));
    if written.is_err() {
        remove_with_sidecars(&tmp);
    }
    written
}

/// States 2 and 3: fills `tmp` from `src`, checks it, closes and syncs it.
fn fill(
    src: &Connection,
    tmp: &Path,
    prepare: impl FnOnce(&Connection) -> rusqlite::Result<()>,
) -> std::result::Result<BackupReport, Box<dyn std::error::Error>> {
    let mut dst = Connection::open(tmp)?;
    prepare(&dst)?;
    {
        let backup = Backup::new(src, &mut dst)?;
        // All pages in one step: one read transaction on the source, so one
        // committed state. A step that can't get its lock copies nothing and
        // is tried again, for as long as a write would wait.
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let mut pause = Duration::from_millis(5);
        loop {
            match backup.step(-1)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked if Instant::now() < deadline => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(100));
                }
                _ => return Err("the workspace stayed locked by another lcl".into()),
            }
        }
    }
    let check: String = dst.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(format!("the copy failed its integrity check: {check}").into());
    }
    let schema_version: i64 = dst.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    dst.close().map_err(|(_, e)| e)?;
    let file = std::fs::OpenOptions::new().write(true).open(tmp)?;
    file.sync_all()?;
    Ok(BackupReport {
        schema_version,
        bytes: file.metadata()?.len(),
    })
}

/// State 4: gives the checked file its name, without ever replacing one.
fn place(tmp: &Path, dest: &Path, shown: &str) -> Result<()> {
    let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    // A hard link fails if `dest` exists, so no file is ever replaced.
    let placed = match std::fs::hard_link(tmp, dest) {
        Ok(()) => std::fs::remove_file(tmp),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        // A filesystem without hard links: check, then rename.
        Err(_) if exists(dest) => Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)),
        Err(_) => std::fs::rename(tmp, dest),
    };
    placed.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            LclError::BackupExists(shown.to_string())
        } else {
            LclError::BackupFailed {
                path: shown.to_string(),
                reason: e.to_string(),
            }
        }
    })
}

/// Removes a temporary database and whatever SQLite may have put beside it.
fn remove_with_sidecars(tmp: &Path) {
    let _ = std::fs::remove_file(tmp);
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut side = tmp.as_os_str().to_owned();
        side.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(side));
    }
}

impl crate::db::Store {
    /// Copies this workspace to a new file at `dest`, safely while other lcl
    /// processes read and write it. An existing `dest` is refused.
    pub fn backup(&self, dest: &Path) -> Result<BackupReport> {
        copy_database(&self.conn, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Store;
    use std::collections::BTreeSet;

    /// A directory of its own, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("lcl-backup-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }

        fn names(&self) -> BTreeSet<String> {
            std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A workspace in `dir` with `n` rows in a table of its own.
    fn source(dir: &Dir, n: i64) -> Store {
        let store = Store::open(&dir.0.join("src.sqlite")).unwrap();
        store
            .conn
            .execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            .unwrap();
        for i in 0..n {
            store
                .conn
                .execute("INSERT INTO t (body) VALUES (?1)", [format!("row {i}")])
                .unwrap();
        }
        store
    }

    fn rows(path: &Path) -> i64 {
        let conn = Connection::open(path).unwrap();
        conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap()
    }

    /// Whole or absent, and no litter: a finished copy adds exactly `DEST`.
    #[test]
    fn a_copy_adds_one_file_and_it_is_the_source() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 50);
        let dest = out.0.join("copy.sqlite");
        let report = store.backup(&dest).unwrap();
        assert_eq!(out.names(), BTreeSet::from(["copy.sqlite".to_string()]));
        assert_eq!(report.schema_version, crate::SCHEMA_VERSION);
        assert_eq!(report.bytes, std::fs::metadata(&dest).unwrap().len());
        assert_eq!(rows(&dest), 50);
    }

    /// Never replaced: any existing `DEST` is refused and keeps its bytes.
    #[test]
    fn an_existing_destination_is_refused_and_untouched() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 5);
        let dest = out.0.join("taken");
        std::fs::write(&dest, b"mine").unwrap();
        let err = store.backup(&dest).unwrap_err();
        assert!(matches!(err, LclError::BackupExists(_)), "{err:?}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"mine");
        assert_eq!(out.names(), BTreeSet::from(["taken".to_string()]));
        // A directory counts as existing too.
        let err = store.backup(&out.0).unwrap_err();
        assert!(matches!(err, LclError::BackupExists(_)), "{err:?}");
    }

    /// One committed state: a write another connection has open is not in
    /// the copy and does not hold it up, and the write still commits.
    #[test]
    fn an_open_write_elsewhere_is_left_out_and_not_blocked() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 10);
        let mut writer = Connection::open(dir.0.join("src.sqlite")).unwrap();
        let tx = writer.transaction().unwrap();
        tx.execute("INSERT INTO t (body) VALUES ('uncommitted')", [])
            .unwrap();
        let before = out.0.join("before.sqlite");
        store.backup(&before).unwrap();
        tx.commit().unwrap();
        let after = out.0.join("after.sqlite");
        store.backup(&after).unwrap();
        assert_eq!((rows(&before), rows(&after)), (10, 11));
    }

    /// A destination that fills up: the error names the path, and nothing
    /// is left, not at `DEST` and not beside it.
    #[test]
    fn a_full_destination_leaves_nothing() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 2000);
        let dest = out.0.join("copy.sqlite");
        // SQLite reports a full disk and a page limit the same way: SQLITE_FULL.
        let err = copy_database_with(&store.conn, &dest, |dst| {
            dst.pragma_update(None, "max_page_count", 2)
        })
        .unwrap_err();
        let LclError::BackupFailed { path, reason } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(path, &dest.display().to_string());
        assert!(reason.contains("full"), "{reason}");
        assert_eq!(out.names(), BTreeSet::new());
    }

    /// A destination directory that isn't there: the same, and none is made.
    #[test]
    fn a_missing_directory_leaves_nothing() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 5);
        let dest = out.0.join("no-such-dir").join("copy.sqlite");
        let err = store.backup(&dest).unwrap_err();
        assert!(matches!(err, LclError::BackupFailed { .. }), "{err:?}");
        assert_eq!(out.names(), BTreeSet::new());
    }

    /// Whole or absent, before placing: while the copy is being filled and
    /// checked, `DEST` does not exist, and the file that does is not `DEST`.
    #[test]
    fn nothing_is_at_the_destination_until_the_copy_is_checked() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 50);
        let dest = out.0.join("copy.sqlite");
        let tmp = out.0.join(".copy.sqlite.lcl-backup-test.tmp");
        fill(&store.conn, &tmp, |_| Ok(())).unwrap();
        assert!(!dest.exists());
        assert_eq!(
            out.names(),
            BTreeSet::from([".copy.sqlite.lcl-backup-test.tmp".to_string()])
        );
        // A file that appears at DEST meanwhile wins: placing never replaces.
        std::fs::write(&dest, b"mine").unwrap();
        let err = place(&tmp, &dest, "copy.sqlite").unwrap_err();
        assert!(matches!(err, LclError::BackupExists(_)), "{err:?}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"mine");
    }

    /// Source untouched: the source's bytes and its rows are as they were.
    #[test]
    fn the_source_is_only_read() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 20);
        let data_version = |s: &Store| -> i64 {
            s.conn
                .query_row("PRAGMA data_version", [], |r| r.get(0))
                .unwrap()
        };
        let observer = Store::open(&dir.0.join("src.sqlite")).unwrap();
        let before = data_version(&observer);
        store.backup(&out.0.join("copy.sqlite")).unwrap();
        // data_version changes when another connection commits a write.
        assert_eq!(data_version(&observer), before);
        assert_eq!(rows(&dir.0.join("src.sqlite")), 20);
    }
}
