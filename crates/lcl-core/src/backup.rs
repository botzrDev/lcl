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
//! 4. **Placed.** `DEST` is that file, and the temporary name is gone. On
//!    Unix the directory is synced too, so `DEST` is there after a power
//!    loss.
//!
//! An error in 2 or 3 removes the temporary file and reports the path. A
//! process killed in 2 or 3 leaves the temporary file behind (nothing can
//! remove it), and still nothing at `DEST`. An error names `DEST` and the
//! reason, never the temporary name. A lock not got in time is
//! `LclError::BackupLocked`, kind busy, as for a write; anything else is
//! kind io.
//!
//! # Invariants
//!
//! - **Whole or absent.** `DEST` either does not exist or is a complete
//!   database. No reader ever finds part of one there, also not after a
//!   kill, a full disk or an unwritable directory.
//! - **Never replaced.** A file that exists at `DEST`, of any kind, is left
//!   byte for byte as it was. Placing is a hard link, which fails if `DEST`
//!   has appeared since state 1. (On a filesystem with no hard links, as
//!   FAT, placing is a check and then a rename: a file that appears at
//!   `DEST` between the two is replaced. That window is the one exception.)
//! - **No wider than the source.** The copy has the source file's
//!   permissions from before the first page is written, so a workspace only
//!   its owner can read is never copied to a file others can.
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

//!
//! # The snapshot before an upgrade
//!
//! Before a migration changes a workspace file or the registry, lcl copies
//! the file beside itself with the same function. For `FILE` at schema `N`
//! the copy is `FILE.vN.snapshot`, or `FILE.vN.snapshot.2`, `.3`, … when
//! that name is taken. `db::migrate` and `registry::migrate` call
//! `snapshot_before_upgrade` and hold these:
//!
//! - **Snapshot first.** No migration step runs unless a snapshot is whole
//!   and placed. A snapshot that can't be written stops the upgrade, and
//!   the file is as it was.
//! - **Exactly the state upgraded.** The snapshot is taken while the
//!   migration's connection holds the write lock, after it read the version
//!   again under that lock and before it has written anything. The copy is
//!   read through a second connection (SQLite won't back up through one
//!   that is writing), which in WAL mode sees the last commit: the state
//!   the first step starts from. No other process can write or migrate
//!   between the copy and the first step. The copy's own version is read
//!   back and must be `N`; a copy at any other version is discarded and
//!   the upgrade stops.
//! - **One upgrade, one snapshot.** Processes that open an old file at once
//!   queue on the write lock. The first takes the snapshot and migrates;
//!   the rest find the file current and take none.
//! - **Never overwritten.** An existing snapshot keeps its bytes; a later
//!   one gets the next free name.
//! - **Kept only if the upgrade commits.** A migration that fails rolls
//!   back, the file is unchanged, and the snapshot it took is removed.
//!   (A process killed between the snapshot and the commit leaves it.)
//! - **Nothing for nothing.** A file that is current takes no snapshot and
//!   no write lock. A file with no tables, which is a new one, takes no
//!   snapshot.

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
    copy_database_with(src, dest, BUSY_TIMEOUT, |_| Ok(()), |_| Ok(()))
}

/// What can stop a copy before it is placed.
type Stop = Box<dyn std::error::Error>;

/// The copy waited `wait` for a lock and did not get it. Reported as
/// `LclError::BackupLocked`, kind busy, as a write that times out on the
/// lock is, and not as a file that can't be written.
#[derive(Debug)]
struct StayedLocked;

impl std::fmt::Display for StayedLocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the database stayed locked")
    }
}

impl std::error::Error for StayedLocked {}

/// `copy_database`, waiting up to `wait` for a lock, with two hooks on the
/// temporary file's connection: `prepare` runs before any page is copied (a
/// test uses it to make the copy fail part-way), and `check` runs on the
/// finished copy before it is placed, so a copy it refuses never gets its
/// name.
fn copy_database_with(
    src: &Connection,
    dest: &Path,
    wait: Duration,
    prepare: impl FnOnce(&Connection) -> rusqlite::Result<()>,
    check: impl FnOnce(&Connection) -> std::result::Result<(), Stop>,
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

    let written = fill(src, &tmp, wait, prepare, check)
        .map_err(|e| {
            if e.is::<StayedLocked>() {
                LclError::BackupLocked {
                    path: shown.clone(),
                    file: src.path().unwrap_or_default().to_string(),
                }
            } else {
                failed(e.to_string())
            }
        })
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
    wait: Duration,
    prepare: impl FnOnce(&Connection) -> rusqlite::Result<()>,
    check: impl FnOnce(&Connection) -> std::result::Result<(), Stop>,
) -> std::result::Result<BackupReport, Stop> {
    // Made here, not by SQLite, so a directory that is missing or can't be
    // written is reported by the OS, without the temporary name.
    if let Err(e) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
    {
        let dir = tmp.parent().unwrap_or(Path::new("."));
        return Err(format!("can't create a file in {}: {e}", dir.display()).into());
    }
    let mut dst = Connection::open(tmp)?;
    // Before any page: SQLite made the file with the default mode, and it
    // is still empty.
    if let Some(source) = src.path().filter(|p| !p.is_empty()) {
        match_permissions(Path::new(source), tmp)?;
    }
    prepare(&dst)?;
    {
        let backup = Backup::new(src, &mut dst)?;
        // All pages in one step: one read transaction on the source, so one
        // committed state. A step that can't get its lock copies nothing and
        // is tried again, for `wait`: as long as a write would wait.
        let deadline = Instant::now() + wait;
        let mut pause = Duration::from_millis(5);
        loop {
            match backup.step(-1)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked if Instant::now() < deadline => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(100));
                }
                _ => return Err(Box::new(StayedLocked)),
            }
        }
    }
    let integrity: String = dst.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(format!("the copy failed its integrity check: {integrity}").into());
    }
    let schema_version: i64 = dst.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    check(&dst)?;
    dst.close().map_err(|(_, e)| e)?;
    let file = std::fs::OpenOptions::new().write(true).open(tmp)?;
    file.sync_all()?;
    Ok(BackupReport {
        schema_version,
        bytes: file.metadata()?.len(),
    })
}

/// Gives `copy` the permissions of `source`, where files have an owner and
/// a mode. Elsewhere a file inherits its directory's access list, which the
/// copy, made beside the source or where the user asked, already has.
fn match_permissions(source: &Path, copy: &Path) -> std::io::Result<()> {
    if cfg!(unix) {
        std::fs::set_permissions(copy, std::fs::metadata(source)?.permissions())?;
    }
    Ok(())
}

/// State 4: gives the checked file its name, without ever replacing one.
fn place(tmp: &Path, dest: &Path, shown: &str) -> Result<()> {
    let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    // A hard link fails if `dest` exists, so no file is ever replaced.
    let placed = match std::fs::hard_link(tmp, dest) {
        // Placed: `dest` is the whole file whatever becomes of the other
        // name, so a temporary name that can't be removed (held open by a
        // scanner, say) is litter, not a failed copy.
        Ok(()) => {
            let _ = std::fs::remove_file(tmp);
            sync_dir(dest);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        // A filesystem without hard links: check, then rename.
        Err(_) if exists(dest) => Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)),
        Err(_) => std::fs::rename(tmp, dest).inspect(|()| sync_dir(dest)),
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

/// Syncs the directory `placed` is in, so the name it was just given is on
/// disk too, and a copy reported as written is there after a power loss.
/// Unix only: elsewhere a directory can't be opened to sync it. An error is
/// ignored, as SQLite ignores one syncing a directory: some filesystems
/// refuse it, and the copy is placed and whole either way.
fn sync_dir(placed: &Path) {
    if cfg!(unix)
        && let Some(dir) = placed.parent()
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(dir) = std::fs::File::open(dir) {
            let _ = dir.sync_all();
        }
    }
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

/// Copies the database file `file`, at version `version`, to a snapshot
/// beside it, and returns the snapshot's path. `version_of` reads a
/// database's version the way the caller does (a workspace's is
/// `user_version`; the registry keeps its own in a table).
///
/// Called while the caller's connection holds the migration's write lock on
/// `file` and has written nothing, with the file in WAL mode. See the module
/// documentation for the rules.
pub(crate) fn snapshot_before_upgrade(
    file: &Path,
    version: i64,
    version_of: fn(&Connection) -> Result<i64>,
) -> Result<PathBuf> {
    let failed = |snapshot: &Path, reason: String| LclError::SnapshotFailed {
        file: file.display().to_string(),
        snapshot: snapshot.display().to_string(),
        reason,
    };
    let opened = Connection::open(file).and_then(|src| {
        src.busy_timeout(BUSY_TIMEOUT)?;
        Ok(src)
    });
    let src = match opened {
        Ok(src) => src,
        Err(e) => return Err(failed(file, format!("can't read the file: {e}"))),
    };
    let base = {
        let mut name = file
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(format!(".v{version}.snapshot"));
        file.with_file_name(name)
    };
    // The first name no file has. `copy_database` refuses a taken name
    // itself, also one taken since this looked, so nothing is overwritten.
    for n in 1..=MAX_SNAPSHOTS {
        let snapshot = if n == 1 {
            base.clone()
        } else {
            let mut name = base.clone().into_os_string();
            name.push(format!(".{n}"));
            PathBuf::from(name)
        };
        let copied = copy_database_with(
            &src,
            &snapshot,
            BUSY_TIMEOUT,
            |_| Ok(()),
            |copy| {
                match version_of(copy)? {
                found if found == version => Ok(()),
                found => Err(format!(
                    "the copy is at version {found}, not {version}: the file changed while it was copied"
                )
                .into()),
            }
            },
        );
        match copied {
            Ok(_) => return Ok(snapshot),
            Err(LclError::BackupExists(_)) => continue,
            // Kind busy, as a write that times out on the lock is. The
            // upgrade has not started.
            Err(locked @ LclError::BackupLocked { .. }) => return Err(locked),
            Err(LclError::BackupFailed { reason, .. }) => return Err(failed(&snapshot, reason)),
            Err(other) => return Err(failed(&snapshot, other.to_string())),
        }
    }
    Err(failed(
        &base,
        format!("{MAX_SNAPSHOTS} snapshots of this version are already there"),
    ))
}

/// How many snapshots of one file at one version may pile up before lcl
/// refuses to add another. Each needs a killed upgrade to come about.
const MAX_SNAPSHOTS: u32 = 100;

/// Removes a snapshot this process took, after its upgrade rolled back.
pub(crate) fn discard_snapshot(snapshot: &Path) {
    let _ = std::fs::remove_file(snapshot);
}

/// Whether the database has no tables, indexes or anything else: a file
/// lcl has just created.
pub(crate) fn is_new(conn: &Connection) -> Result<bool> {
    let objects: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
    Ok(objects == 0)
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
        let err = copy_database_with(
            &store.conn,
            &dest,
            BUSY_TIMEOUT,
            |dst| dst.pragma_update(None, "max_page_count", 2),
            |_| Ok(()),
        )
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
        let missing = out.0.join("no-such-dir");
        let dest = missing.join("copy.sqlite");
        let err = store.backup(&dest).unwrap_err();
        let LclError::BackupFailed { path, reason } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(path, &dest.display().to_string());
        // The OS's reason, naming the directory, not the temporary file.
        assert!(
            reason.starts_with(&format!("can't create a file in {}: ", missing.display()))
                && !reason.contains(".copy.sqlite."),
            "{reason}"
        );
        assert_eq!(err.kind(), crate::error::ErrorKind::Io);
        assert_eq!(out.names(), BTreeSet::new());
    }

    /// A copy that can't get a lock in time is kind busy, as a write that
    /// times out on the lock is, and leaves nothing. The source here is a
    /// rollback-journal file another connection holds an exclusive lock on,
    /// which keeps every reader out.
    #[test]
    fn a_copy_that_stays_locked_is_kind_other() {
        let dir = Dir::new();
        let out = Dir::new();
        let file = dir.0.join("rollback.sqlite");
        let src = Connection::open(&file).unwrap();
        src.execute_batch("CREATE TABLE t (body TEXT); INSERT INTO t VALUES ('row')")
            .unwrap();
        src.busy_timeout(Duration::ZERO).unwrap();
        let holder = Connection::open(&file).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let dest = out.0.join("copy.sqlite");
        let err = copy_database_with(
            &src,
            &dest,
            Duration::from_millis(50),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap_err();
        holder.execute_batch("COMMIT").unwrap();
        assert!(matches!(err, LclError::BackupLocked { .. }), "{err:?}");
        assert_eq!(err.kind(), crate::error::ErrorKind::Busy);
        assert_eq!(
            err.to_string(),
            format!(
                "can't write {}: {} stayed locked by another lcl; run the command again",
                dest.display(),
                src.path().unwrap()
            )
        );
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
        fill(&store.conn, &tmp, BUSY_TIMEOUT, |_| Ok(()), |_| Ok(())).unwrap();
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

    fn user_version(conn: &Connection) -> Result<i64> {
        Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// The snapshot is the file, at the version asked for, under the name
    /// for that version; a second one takes the next name, and the first
    /// keeps its bytes.
    #[test]
    fn a_snapshot_is_named_for_its_version_and_never_overwritten() {
        let dir = Dir::new();
        let store = source(&dir, 30);
        let file = dir.0.join("src.sqlite");
        let v = crate::SCHEMA_VERSION;
        let first = snapshot_before_upgrade(&file, v, user_version).unwrap();
        assert_eq!(first, dir.0.join(format!("src.sqlite.v{v}.snapshot")));
        assert_eq!(rows(&first), 30);
        let bytes = std::fs::read(&first).unwrap();

        store
            .conn
            .execute("INSERT INTO t (body) VALUES ('later')", [])
            .unwrap();
        let second = snapshot_before_upgrade(&file, v, user_version).unwrap();
        assert_eq!(second, dir.0.join(format!("src.sqlite.v{v}.snapshot.2")));
        assert_eq!(rows(&second), 31);
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
    }

    /// Exactly the state upgraded: taken while another connection holds the
    /// write lock with a change not yet committed, the snapshot is the last
    /// commit, and the lock holder is not disturbed.
    #[test]
    fn a_snapshot_under_the_write_lock_is_the_last_commit() {
        let dir = Dir::new();
        let mut store = source(&dir, 10);
        let file = dir.0.join("src.sqlite");
        let tx = store
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let snapshot = snapshot_before_upgrade(&file, crate::SCHEMA_VERSION, user_version).unwrap();
        tx.execute("INSERT INTO t (body) VALUES ('a migration step')", [])
            .unwrap();
        tx.commit().unwrap();
        assert_eq!((rows(&snapshot), rows(&file)), (10, 11));
    }

    /// A copy that turns out to be at another version is discarded: no
    /// snapshot is left, and the caller is told, so it does not upgrade.
    #[test]
    fn a_copy_at_another_version_is_discarded() {
        let dir = Dir::new();
        let _store = source(&dir, 5);
        let file = dir.0.join("src.sqlite");
        let before = dir.names();
        let err =
            snapshot_before_upgrade(&file, crate::SCHEMA_VERSION - 1, user_version).unwrap_err();
        let LclError::SnapshotFailed { reason, .. } = &err else {
            panic!("{err:?}");
        };
        assert!(
            reason.contains("the file changed while it was copied"),
            "{reason}"
        );
        assert_eq!(dir.names(), before);
    }

    /// A snapshot that can't be written is an error that names the file and
    /// the snapshot, and leaves nothing. Here the temporary name is too long
    /// for a file name (255 bytes). Windows limits the whole path instead,
    /// so the long name can't be made there.
    #[cfg(unix)]
    #[test]
    fn a_snapshot_that_cannot_be_written_is_an_error_and_leaves_nothing() {
        let dir = Dir::new();
        let name = format!("{}.sqlite", "w".repeat(230));
        let file = dir.0.join(&name);
        let store = Store::open(&file).unwrap();
        let before = dir.names();
        let err = snapshot_before_upgrade(&file, crate::SCHEMA_VERSION, user_version).unwrap_err();
        let LclError::SnapshotFailed {
            file: named,
            snapshot,
            ..
        } = &err
        else {
            panic!("{err:?}");
        };
        assert_eq!(named, &file.display().to_string());
        assert!(snapshot.ends_with(&format!(".v{}.snapshot", crate::SCHEMA_VERSION)));
        assert!(err.to_string().contains("nothing was changed"), "{err}");
        assert_eq!(err.kind(), crate::error::ErrorKind::Io);
        assert_eq!(dir.names(), before);
        drop(store);
    }

    /// Nothing for nothing: a database with no tables is new.
    #[test]
    fn only_a_database_with_nothing_in_it_is_new() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(is_new(&conn).unwrap());
        conn.execute_batch("CREATE TABLE t (x)").unwrap();
        assert!(!is_new(&conn).unwrap());
    }

    /// No wider than the source: a copy and a snapshot of a file only its
    /// owner can read are files only its owner can read.
    #[cfg(unix)]
    #[test]
    fn a_copy_has_the_source_s_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 5);
        let file = dir.0.join("src.sqlite");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        for wanted in [0o600, 0o640] {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(wanted)).unwrap();
            let copy = out.0.join(format!("copy-{wanted:o}.sqlite"));
            store.backup(&copy).unwrap();
            let snapshot =
                snapshot_before_upgrade(&file, crate::SCHEMA_VERSION, user_version).unwrap();
            assert_eq!((mode(&copy), mode(&snapshot)), (wanted, wanted));
        }
    }

    /// A temporary name that can't be removed after placing is not a failed
    /// copy: the destination is whole and the call succeeds.
    #[test]
    fn a_placed_copy_is_a_success_whatever_becomes_of_the_temporary_name() {
        let dir = Dir::new();
        let out = Dir::new();
        let store = source(&dir, 5);
        let tmp = out.0.join(".copy.tmp");
        fill(&store.conn, &tmp, BUSY_TIMEOUT, |_| Ok(()), |_| Ok(())).unwrap();
        let dest = out.0.join("copy.sqlite");
        place(&tmp, &dest, "copy.sqlite").unwrap();
        assert_eq!(rows(&dest), 5);
        // Placing again from a name that is gone fails without touching it.
        let bytes = std::fs::read(&dest).unwrap();
        assert!(place(&tmp, &dest, "copy.sqlite").is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    }
}
