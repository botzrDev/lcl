//! The workspace registry: `~/.lcl/index.sqlite`, the workspaces
//! `lcl workspace list` shows, `lcl workspace create` and `lcl init` add to,
//! and `--workspace` looks names up in.
//!
//! A workspace is global (its file is `~/.lcl/projects/<slug>.sqlite`, found
//! from the current HOME, so the registry survives HOME moving) or cwd (made
//! by `lcl init` at a directory's `.lcl/data.sqlite`, found at the path
//! recorded for it). Names are unique across both kinds, by slug, and so are
//! paths.
//!
//! Only this module opens the index. It isn't a workspace database: it has
//! its own schema, versioned by a `registry_meta` row, not by
//! `PRAGMA user_version`. Builds from c546b8a to 097af5c opened the index as
//! if it were a workspace (`Store::open`), which ran the workspace migrations
//! on it and left user_version as high as 7; that number says nothing about
//! the registry. The tables those builds and lcl 0.1.0 left in the file
//! (`projects` and the workspace tables) stay there, and are never read or
//! written again, except that the registry's V1 copies the `projects` rows.

use crate::db::{BUSY_TIMEOUT, Store, retry_busy, use_wal};
use crate::error::{LclError, Result};
use crate::models::now;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// The registry schema this build writes, kept in `registry_meta`.
pub const REGISTRY_VERSION: i64 = MIGRATIONS.len() as i64;

type Migration = fn(&Transaction<'_>) -> Result<()>;

/// Registry migrations: entry `n` takes the registry from vn to v(n+1).
const MIGRATIONS: &[Migration] = &[v1, v2];

/// The home directory, where lcl keeps `.lcl`, from the two environment
/// variables that can name it. A pure function of its arguments: the
/// caller reads the environment and says whether this is Windows.
///
/// - `HOME`, when it is set and not empty, on every platform. A registry
///   that an earlier lcl made under `HOME` stays where it is, on Windows
///   too (Git Bash and MSYS set `HOME` there).
/// - Else, on Windows only, `USERPROFILE`, when it is set and not empty:
///   cmd and PowerShell set it and not `HOME`.
/// - Else an error that names both. Never the current directory, a
///   temporary directory, or any other guess: a registry in a place the
///   next run would not look is worse than none.
pub fn home_dir(
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    windows: bool,
) -> Result<PathBuf> {
    let given = |v: Option<std::ffi::OsString>| v.filter(|v| !v.is_empty());
    given(home)
        .or_else(|| given(userprofile).filter(|_| windows))
        .map(PathBuf::from)
        .ok_or(LclError::NoHome)
}

/// lcl's global directory, `<home>/.lcl`.
pub fn lcl_dir(home: &Path) -> PathBuf {
    home.join(".lcl")
}

/// Where global workspace files live: `<home>/.lcl/projects/<slug>.sqlite`.
/// The directory is called `projects` because lcl 0.1.0 called workspaces
/// projects and put their files there; they stay where they are.
pub fn workspaces_dir(home: &Path) -> PathBuf {
    lcl_dir(home).join("projects")
}

/// The registry's own file.
pub fn index_path(home: &Path) -> PathBuf {
    lcl_dir(home).join("index.sqlite")
}

/// The file of the global workspace with this slug.
pub fn workspace_path(home: &Path, slug: &str) -> PathBuf {
    workspaces_dir(home).join(format!("{slug}.sqlite"))
}

/// A workspace name's slug, which names its file: letters and digits kept
/// (ASCII ones lowercased), everything else `-`, and `-` trimmed from both
/// ends. This is lcl 0.1.0's rule, so its files are still found. None when
/// nothing is left. No `/` or `.` survives, so a slug can't leave the
/// workspaces directory.
pub fn slug(name: &str) -> Option<String> {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    (!slug.is_empty()).then(|| slug.to_string())
}

/// A workspace's issue key, the DRM in DRM-12: an ASCII letter, then up to
/// nine ASCII letters or digits, uppercase. Parsing uppercases ASCII only, so
/// a non-ASCII letter is refused rather than turned into something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceKey(String);

impl WorkspaceKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for WorkspaceKey {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let key = s.to_ascii_uppercase();
        let mut chars = key.chars();
        let valid = key.len() <= 10
            && chars.next().is_some_and(|c| c.is_ascii_uppercase())
            && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        if valid {
            Ok(WorkspaceKey(key))
        } else {
            Err(format!(
                "invalid key {s:?}: a key is a letter, then up to 9 letters or digits, such as DRM"
            ))
        }
    }
}

impl fmt::Display for WorkspaceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a registered workspace's file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// `~/.lcl/projects/<slug>.sqlite`, under the current HOME, whatever
    /// path was recorded when it was made.
    Global,
    /// The `.lcl/data.sqlite` that `lcl init` made, at its recorded path.
    Cwd,
}

impl Kind {
    fn parse(s: &str) -> Result<Kind> {
        match s {
            "global" => Ok(Kind::Global),
            "cwd" => Ok(Kind::Cwd),
            other => Err(LclError::UnknownWorkspaceKind(other.to_string())),
        }
    }
}

/// One registered workspace.
#[derive(Debug, Clone, Serialize)]
pub struct RegisteredWorkspace {
    pub name: String,
    pub slug: String,
    pub kind: Kind,
    /// Where its file is: for a global workspace, under the current HOME.
    pub path: String,
    pub created_at: i64,
    /// Nothing is at `path` any more.
    pub missing: bool,
}

/// What `init_workspace` found or made: the same keys as `NewWorkspace`.
#[derive(Debug, Clone, Serialize)]
pub struct InitWorkspace {
    pub name: String,
    pub slug: String,
    pub key: String,
    pub path: String,
    pub created_at: i64,
    /// False when the workspace was already there.
    pub created: bool,
}

/// The file of a global workspace `--workspace NAME` names, or else of the
/// registered workspace NAME, for `slug`: the registry's answer when there
/// is an index (which this opens, and so upgrades), else, and for a slug
/// with no row, `~/.lcl/projects/<slug>.sqlite`. No index is created.
/// With the file comes `Registry::upgrade_note`, for the caller to show.
pub fn resolve(home: &Path, slug: &str) -> Result<(PathBuf, Option<String>)> {
    let (found, note) = if index_path(home).exists() {
        let registry = Registry::open(home)?;
        (registry.find(slug)?, registry.upgrade_note)
    } else {
        (None, None)
    };
    Ok((found.unwrap_or_else(|| workspace_path(home, slug)), note))
}

/// A workspace `create_workspace` made.
#[derive(Debug, Clone, Serialize)]
pub struct NewWorkspace {
    pub name: String,
    pub slug: String,
    pub key: String,
    pub path: String,
    pub created_at: i64,
    /// Always true. `lcl init` prints the same keys, and there it is false
    /// when the workspace was already there.
    pub created: bool,
}

pub struct Registry {
    conn: Connection,
    home: PathBuf,
    upgrade_note: Option<String>,
}

impl Registry {
    /// Opens (creating if need be) `<home>/.lcl/index.sqlite` and brings its
    /// registry schema up to date. The order matches `Store::init`: the busy
    /// timeout first, then the version check, before anything can write.
    pub fn open(home: &Path) -> Result<Self> {
        let path = index_path(home);
        std::fs::create_dir_all(lcl_dir(home))?;
        retry_busy(|| {
            let mut conn = Connection::open(&path)?;
            conn.busy_timeout(BUSY_TIMEOUT)?;
            check_supported(&path, registry_version(&conn)?)?;
            use_wal(&conn)?;
            let upgrade_note = migrate(&mut conn, &path)?;
            Ok(Registry {
                conn,
                home: home.to_path_buf(),
                upgrade_note,
            })
        })
    }

    /// When this open upgraded a registry an older lcl wrote, one line
    /// saying so and naming the snapshot taken first. Else None. As
    /// `Store::upgrade_note`: whoever opens the registry for a person shows
    /// it, on stderr.
    pub fn upgrade_note(&self) -> Option<&str> {
        self.upgrade_note.as_deref()
    }

    /// The registry's schema version.
    pub fn schema_version(&self) -> Result<i64> {
        registry_version(&self.conn)
    }

    /// Every registered workspace, by name.
    pub fn list(&self) -> Result<Vec<RegisteredWorkspace>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, slug, kind, path, created_at FROM workspaces ORDER BY name, slug",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (name, slug, kind, recorded, created_at) = row?;
            let kind = Kind::parse(&kind)?;
            let path = file_of(&self.home, kind, &slug, &recorded);
            let missing = std::fs::symlink_metadata(&path).is_err();
            out.push(RegisteredWorkspace {
                name,
                slug,
                kind,
                path: path.display().to_string(),
                created_at,
                missing,
            });
        }
        Ok(out)
    }

    /// The file of the workspace registered with `slug`, if one is.
    pub fn find(&self, slug: &str) -> Result<Option<PathBuf>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT kind, path FROM workspaces WHERE slug = ?1",
                params![slug],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(kind, recorded)| Ok(file_of(&self.home, Kind::parse(&kind)?, slug, &recorded)))
            .transpose()
    }

    /// `lcl init`: the cwd workspace `name`, with key `key`, at `path` (the
    /// current directory's `.lcl/data.sqlite`, recorded as given).
    ///
    /// Under the registry's write lock, in this order:
    /// - the name's slug may be registered only to `path`; else nothing is
    ///   made, not even `path`'s directory;
    /// - a file already at `path` is opened with `open` and left as it is:
    ///   with this name and key, init has nothing to do but register it if
    ///   no row has its path (which writes only the index); with another
    ///   name or key it is refused, never renamed or re-keyed;
    /// - else the file is made, as `create_workspace` makes one (built under
    ///   a temporary name, renamed into place, its row committed last), and
    ///   a row left for `path` by a file that is gone is replaced.
    ///
    /// Either way, once the name and key are good and before anything is
    /// registered, `.gitignore` is written beside the file if nothing is
    /// there by that name (`write_gitignore`). If it can't be, init fails:
    /// no workspace is made, no row is written, and a directory init made
    /// is removed. A failure later in making a workspace removes the
    /// `.gitignore` it wrote too.
    pub fn init_workspace(
        &mut self,
        name: &str,
        key: &WorkspaceKey,
        path: &Path,
        open: impl Fn(&Path) -> Result<Store>,
    ) -> Result<InitWorkspace> {
        let slug = slug(name).ok_or_else(|| LclError::NoSlug(name.to_string()))?;
        let recorded = path.to_string_lossy().into_owned();
        let home = self.home.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let holder: Option<(String, String, String)> = tx
            .query_row(
                "SELECT name, kind, path FROM workspaces WHERE slug = ?1",
                params![slug],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((existing, kind, at)) = holder
            && at != recorded
        {
            let at = file_of(&home, Kind::parse(&kind)?, &slug, &at)
                .display()
                .to_string();
            return Err(if existing == name {
                LclError::AlreadyRegistered {
                    name: existing,
                    path: at,
                }
            } else {
                LclError::NameTaken {
                    name: name.to_string(),
                    existing,
                    path: at,
                }
            });
        }
        let path_row: Option<String> = tx
            .query_row(
                "SELECT slug FROM workspaces WHERE path = ?1",
                params![recorded],
                |r| r.get(0),
            )
            .optional()?;
        let found = |created, created_at| InitWorkspace {
            name: name.to_string(),
            slug: slug.clone(),
            key: key.to_string(),
            path: recorded.clone(),
            created_at,
            created,
        };

        let dir = path.parent().ok_or_else(|| LclError::NoParentDir {
            path: path.display().to_string(),
        })?;

        // An empty file is no workspace (lcl never leaves one: it builds a
        // workspace under another name and renames it into place), so it is
        // replaced like a missing one.
        let exists = std::fs::symlink_metadata(path).is_ok_and(|m| !(m.is_file() && m.len() == 0));
        if exists {
            // Opening an old file upgrades it, and `open` may say so, even
            // when init then refuses: an upgrade is neither a rename nor a
            // re-key.
            let store = open(path)?;
            let (have_name, have_key) = (
                store.get_meta("name")?.unwrap_or_default(),
                store.get_meta("key")?.unwrap_or_default(),
            );
            let created_at = store
                .get_meta("created_at")?
                .and_then(|t| t.parse::<i64>().ok())
                .unwrap_or_else(now);
            drop(store);
            if have_name != name || have_key != key.as_str() {
                return Err(LclError::InitMismatch {
                    name: have_name,
                    key: have_key,
                    path: recorded,
                });
            }
            write_gitignore(dir)?;
            if path_row.is_none() {
                tx.execute(
                    "INSERT INTO workspaces(slug, name, path, created_at, kind) VALUES (?1, ?2, ?3, ?4, 'cwd')",
                    params![slug, name, recorded, created_at],
                )?;
                tx.commit()?;
            }
            return Ok(found(false, created_at));
        }

        // No file. A row for this path names a file that is gone.
        if path_row.is_some() {
            tx.execute("DELETE FROM workspaces WHERE path = ?1", params![recorded])?;
        }
        let made_dir = std::fs::symlink_metadata(dir).is_err();
        std::fs::create_dir_all(dir)?;
        let undo_dir = || {
            if made_dir {
                let _ = std::fs::remove_dir(dir);
            }
        };
        let gitignore = write_gitignore(dir).inspect_err(|_| undo_dir())?;
        let undo = || {
            if let Some(gitignore) = &gitignore {
                let _ = std::fs::remove_file(gitignore);
            }
            undo_dir();
        };
        let created_at = now();
        let tmp = dir.join(format!(".data.{}.creating", uuid::Uuid::new_v4()));
        let made = (|| {
            build_workspace(&tmp, name, key, created_at)?;
            tx.execute(
                "INSERT INTO workspaces(slug, name, path, created_at, kind) VALUES (?1, ?2, ?3, ?4, 'cwd')",
                params![slug, name, recorded, created_at],
            )?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        })();
        if let Err(e) = made {
            remove_db_files(&tmp);
            undo();
            return Err(e); // dropping `tx` rolls the row back
        }
        if let Err(e) = tx.commit() {
            remove_db_files(path);
            undo();
            return Err(e.into());
        }
        Ok(found(true, created_at))
    }

    /// Creates the global workspace `name` with issue key `key`: its file at
    /// `~/.lcl/projects/<slug>.sqlite` and its registry row, both or neither.
    ///
    /// Everything runs under the registry's write lock (`BEGIN IMMEDIATE`),
    /// so two creates of one slug can't both pass the checks. The file is
    /// built under a temporary name and renamed into place, so no other
    /// command can open it half made. The row is inserted first and committed
    /// last: an error before the commit rolls the row back and removes the
    /// file.
    pub fn create_workspace(&mut self, name: &str, key: &WorkspaceKey) -> Result<NewWorkspace> {
        let slug = slug(name).ok_or_else(|| LclError::NoSlug(name.to_string()))?;
        let path = workspace_path(&self.home, &slug);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT name FROM workspaces WHERE slug = ?1",
                params![slug],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            return Err(if existing == name {
                LclError::WorkspaceExists(existing)
            } else {
                LclError::SlugTaken {
                    name: name.to_string(),
                    existing,
                    slug,
                }
            });
        }
        // A file nobody registered: refuse rather than overwrite it.
        if std::fs::symlink_metadata(&path).is_ok() {
            return Err(LclError::WorkspaceFileExists(path.display().to_string()));
        }
        let created_at = now();
        let tmp =
            workspaces_dir(&self.home).join(format!(".{slug}.{}.creating", uuid::Uuid::new_v4()));
        let made = (|| {
            build_workspace(&tmp, name, key, created_at)?;
            tx.execute(
                "INSERT INTO workspaces(slug, name, path, created_at, kind) VALUES (?1, ?2, ?3, ?4, 'global')",
                params![slug, name, path.to_string_lossy(), created_at],
            )?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if let Err(e) = made {
            remove_db_files(&tmp);
            return Err(e); // dropping `tx` rolls the row back
        }
        if let Err(e) = tx.commit() {
            remove_db_files(&path);
            return Err(e.into());
        }
        Ok(NewWorkspace {
            name: name.to_string(),
            slug,
            key: key.to_string(),
            path: path.display().to_string(),
            created_at,
            created: true,
        })
    }
}

/// Where a row's file is: a global workspace's under the current HOME, a
/// cwd workspace's where it was recorded.
fn file_of(home: &Path, kind: Kind, slug: &str, recorded: &str) -> PathBuf {
    match kind {
        Kind::Global => workspace_path(home, slug),
        Kind::Cwd => PathBuf::from(recorded),
    }
}

/// Builds a workspace database at `path` and closes it. Closing the last
/// connection checkpoints the WAL into the file and deletes the -wal and
/// -shm files, so the one file holds everything and can be renamed alone.
fn build_workspace(path: &Path, name: &str, key: &WorkspaceKey, created_at: i64) -> Result<()> {
    let mut store = Store::open(path)?;
    let created_at = created_at.to_string();
    store.set_meta(&[
        ("name", name),
        ("key", key.as_str()),
        ("description", ""),
        ("created_at", created_at.as_str()),
    ])?;
    drop(store);
    for ext in ["wal", "shm"] {
        let sidecar = sidecar(path, ext);
        if sidecar.exists() {
            return Err(LclError::WorkspaceLeftOpen {
                path: sidecar.display().to_string(),
            });
        }
    }
    Ok(())
}

fn sidecar(path: &Path, ext: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!("-{ext}"));
    PathBuf::from(name)
}

/// What `lcl init` writes to `.lcl/.gitignore`. `*` keeps everything in
/// `.lcl/` out of git, the workspace's file, its WAL, its snapshots and this
/// file too: a SQLite file does not merge, and one committed while lcl
/// writes it can be torn. `lcl export` is what another person imports.
pub const GITIGNORE: &str =
    "# Written by lcl init. Share a workspace with lcl export, not git.\n*\n";

/// Writes `GITIGNORE` to `dir/.gitignore` unless something is there by that
/// name (a file, a link, anything), which is left as it is. Returns the path
/// when it wrote one. A write that fails part way removes what it wrote.
fn write_gitignore(dir: &Path) -> Result<Option<PathBuf>> {
    use std::io::Write;
    let path = dir.join(".gitignore");
    let failed = |e: std::io::Error| LclError::GitignoreFailed {
        path: path.display().to_string(),
        reason: e.to_string(),
    };
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(e) => return Err(failed(e)),
    };
    if let Err(e) = file.write_all(GITIGNORE.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(failed(e));
    }
    Ok(Some(path))
}

/// Removes a database file and its -wal and -shm files, if they're there.
fn remove_db_files(path: &Path) {
    for p in [
        path.to_path_buf(),
        sidecar(path, "wal"),
        sidecar(path, "shm"),
    ] {
        let _ = std::fs::remove_file(p);
    }
}

/// Whether a workspace with slug `slug` is registered, found without
/// writing anything: the index is never created or migrated, and is opened
/// immutable. A plain read-only open would leave `-wal` and `-shm` files
/// behind (a read-only connection creates them for a WAL database and can't
/// remove them). Immutable reads only the main file, so a row still in
/// another process's WAL is missed: for a hint, that is only a missing hint.
/// An index lcl 0.1.0 (or a build before the registry) left keeps its
/// workspaces in `projects`. No index, one from a newer lcl, or any error
/// reads as false. For hints only.
pub fn is_registered(home: &Path, slug: &str) -> bool {
    let lookup = || -> Result<bool> {
        let path = index_path(home);
        if !path.is_file() {
            return Ok(false);
        }
        let uri = format!(
            "file:{}?immutable=1",
            uri_path(&path).ok_or(LclError::PathNotUtf8)?
        );
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&uri, flags)?;
        let table = match registry_version(&conn)? {
            0 if table_exists(&conn, "projects")? => "projects",
            1..=REGISTRY_VERSION => "workspaces",
            _ => return Ok(false),
        };
        Ok(conn
            .query_row(
                &format!("SELECT 1 FROM {table} WHERE slug = ?1"),
                params![slug],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    lookup().unwrap_or(false)
}

/// `path` for a `file:` URI: every byte but unreserved ones and `/`
/// percent-encoded, so `?`, `#`, `%` and spaces in it stay part of the path.
fn uri_path(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    Some(
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect(),
    )
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The registry schema version: the `registry_meta` row, or 0 for an index
/// that has none yet (new, or written by an older lcl).
fn registry_version(conn: &Connection) -> Result<i64> {
    if !table_exists(conn, "registry_meta")? {
        return Ok(0);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM registry_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match value {
        None => Ok(0),
        Some(v) => v.parse().map_err(|_| LclError::BadRegistryVersion(v)),
    }
}

/// Fails unless this build understands registry version `found`; nothing may
/// write to an index from a newer lcl.
fn check_supported(path: &Path, found: i64) -> Result<usize> {
    if found > REGISTRY_VERSION {
        return Err(LclError::RegistryTooNew {
            path: path.display().to_string(),
            found,
            supported: REGISTRY_VERSION,
        });
    }
    usize::try_from(found).map_err(|_| LclError::BadRegistryVersionNumber(found))
}

/// Brings the registry up to `REGISTRY_VERSION`, all or nothing, as
/// `db::migrate` does for a workspace: an unlocked check, then the write
/// lock and a second check. A registry an older lcl wrote is first copied
/// beside itself, under the rules in `crate::backup`; the line returned
/// names that copy. A new, empty index gets no copy and no line.
fn migrate(conn: &mut Connection, path: &Path) -> Result<Option<String>> {
    if check_supported(path, registry_version(conn)?)? == MIGRATIONS.len() {
        return Ok(None);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let from = check_supported(path, registry_version(&tx)?)?;
    if from == MIGRATIONS.len() {
        return Ok(None);
    }
    let snapshot = if crate::backup::is_new(&tx)? {
        None
    } else {
        Some(crate::backup::snapshot_before_upgrade(
            path,
            from as i64,
            registry_version,
        )?)
    };
    let upgraded = (|| {
        for m in &MIGRATIONS[from..] {
            m(&tx)?;
        }
        tx.execute(
            "INSERT INTO registry_meta(key, value) VALUES ('schema_version', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![REGISTRY_VERSION.to_string()],
        )?;
        Ok(())
    })()
    .and_then(|()| Ok(tx.commit()?));
    match (upgraded, snapshot) {
        (Ok(()), Some(snapshot)) => Ok(Some(format!(
            "upgraded the workspace registry to v{REGISTRY_VERSION}; the file as it was is {}",
            snapshot.display()
        ))),
        (Ok(()), None) => Ok(None),
        (Err(e), snapshot) => {
            if let Some(snapshot) = snapshot {
                crate::backup::discard_snapshot(&snapshot);
            }
            Err(e)
        }
    }
}

/// v0 -> v1: the registry's tables, and the workspaces an older lcl
/// registered (its `projects` table, if there is one), copied once. Where
/// two old rows share a slug, the earlier one is kept. The old tables are
/// left as they are.
fn v1(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS registry_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS workspaces (
             slug TEXT PRIMARY KEY,
             name TEXT NOT NULL,
             path TEXT NOT NULL,
             created_at INTEGER NOT NULL
         );",
    )?;
    if table_exists(tx, "projects")? {
        tx.execute_batch(
            "INSERT OR IGNORE INTO workspaces(slug, name, path, created_at)
             SELECT slug, name, path, CAST(created_at AS INTEGER) FROM projects ORDER BY created_at, rowid;",
        )?;
    }
    Ok(())
}

/// v1 -> v2: each workspace's kind, one row per file, and a unique path.
///
/// - A row is global when its recorded path ends in the components
///   `.lcl/projects/<slug>.sqlite`, else cwd. Components, so a path
///   recorded on Windows, with `\`, is matched there too.
/// - Rows sharing one path (an `init` under another name used to add one)
///   become one: the row whose slug is the slug of the name stored in that
///   file, else the last row inserted. The name is read without opening
///   the file as a workspace, which would upgrade it.
/// - Then a path is on at most one row.
///
/// The tables lcl 0.1.0 reads here (`projects` and its workspace tables)
/// are left as they are, so 0.1.0 still works on the file.
fn v2(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("ALTER TABLE workspaces ADD COLUMN kind TEXT NOT NULL DEFAULT 'cwd' CHECK (kind IN ('global', 'cwd'));")?;
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = tx.prepare("SELECT rowid, slug, path FROM workspaces ORDER BY rowid")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut by_path: Vec<(&str, Vec<(i64, &str)>)> = vec![];
    for (rowid, slug, path) in &rows {
        let global = Path::new(".lcl")
            .join("projects")
            .join(format!("{slug}.sqlite"));
        if Path::new(path).ends_with(&global) {
            tx.execute(
                "UPDATE workspaces SET kind = 'global' WHERE rowid = ?1",
                params![rowid],
            )?;
        }
        match by_path.iter_mut().find(|(p, _)| p == path) {
            Some((_, group)) => group.push((*rowid, slug)),
            None => by_path.push((path, vec![(*rowid, slug)])),
        }
    }
    for (path, group) in by_path.iter().filter(|(_, g)| g.len() > 1) {
        let stored = stored_name(Path::new(path)).and_then(|n| self::slug(&n));
        let keep = group
            .iter()
            .find(|(_, s)| Some(*s) == stored.as_deref())
            .or(group.last())
            .map(|(rowid, _)| *rowid);
        tx.execute(
            "DELETE FROM workspaces WHERE path = ?1 AND rowid IS NOT ?2",
            params![path, keep],
        )?;
    }
    tx.execute_batch("CREATE UNIQUE INDEX workspaces_path ON workspaces(path);")?;
    Ok(())
}

/// The name stored in the workspace file at `path`, read through an
/// immutable read-only connection, so nothing is written (no upgrade, no
/// -wal or -shm file). None if the file is missing or can't be read.
fn stored_name(path: &Path) -> Option<String> {
    let uri = format!("file:{}?immutable=1", uri_path(path)?);
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&uri, flags).ok()?;
    conn.query_row("SELECT value FROM meta WHERE key = 'name'", [], |r| {
        r.get(0)
    })
    .ok()
}

#[cfg(test)]
mod tests {
    /// `home_dir` over every combination of unset, empty and set, on
    /// Windows and elsewhere.
    #[test]
    fn home_dir_takes_home_then_userprofile_on_windows_and_never_guesses() {
        use std::ffi::OsString;
        let states: [(&str, Option<OsString>); 3] = [
            ("unset", None),
            ("empty", Some(OsString::new())),
            ("set", Some(OsString::from("/h"))),
        ];
        let profiles: [(&str, Option<OsString>); 3] = [
            ("unset", None),
            ("empty", Some(OsString::new())),
            ("set", Some(OsString::from("C:\\Users\\u"))),
        ];
        let mut cases = 0;
        for windows in [false, true] {
            for (h, home) in &states {
                for (p, profile) in &profiles {
                    let got = home_dir(home.clone(), profile.clone(), windows);
                    let want = match (*h, *p, windows) {
                        // HOME wins whenever it is set and not empty.
                        ("set", _, _) => Some(PathBuf::from("/h")),
                        // Otherwise USERPROFILE, on Windows only.
                        (_, "set", true) => Some(PathBuf::from("C:\\Users\\u")),
                        _ => None,
                    };
                    let what = format!("HOME {h}, USERPROFILE {p}, windows {windows}");
                    match (got, want) {
                        (Ok(got), Some(want)) => assert_eq!(got, want, "{what}"),
                        (Err(LclError::NoHome), None) => {}
                        (got, want) => panic!("{what}: got {got:?}, want {want:?}"),
                    }
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 18);
        let message = LclError::NoHome.to_string();
        assert!(
            message.contains("HOME") && message.contains("USERPROFILE"),
            "{message}"
        );
    }

    use super::*;

    #[test]
    fn slugs_match_0_1_0() {
        assert_eq!(slug("R&D").as_deref(), Some("r-d"));
        assert_eq!(slug("R-D").as_deref(), Some("r-d"));
        assert_eq!(slug("  My Project!! ").as_deref(), Some("my-project"));
        assert_eq!(slug("../../etc/passwd").as_deref(), Some("etc-passwd"));
        assert_eq!(slug("Café").as_deref(), Some("café"));
        assert_eq!(slug("&&"), None);
        assert_eq!(slug(""), None);
    }

    #[test]
    fn keys() {
        for (given, stored) in [
            ("drm", "DRM"),
            ("A", "A"),
            ("bzr9", "BZR9"),
            ("ABCDEFGHIJ", "ABCDEFGHIJ"),
        ] {
            assert_eq!(
                given.parse::<WorkspaceKey>().map(|k| k.to_string()),
                Ok(stored.to_string()),
                "{given}"
            );
        }
        for bad in ["", "1AB", "AB-C", "ABCDEFGHIJK", "A B", "ß", "straße", "É"] {
            assert!(bad.parse::<WorkspaceKey>().is_err(), "{bad:?} was accepted");
        }
    }
}
