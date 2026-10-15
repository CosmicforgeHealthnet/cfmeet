//! Keyring secret files on disk (operator tooling, §29).
//!
//! A keyring file is the only copy of the key material a running instance has, so a
//! rotation step must never destroy it before the new document exists:
//!
//! * a NEW file is created with `O_EXCL` (never overwrites);
//! * a REPLACEMENT is written to `<path>.tmp` (mode 0600, fsync), then renamed over the
//!   original — the original is never removed first, and a failure at any point leaves
//!   it byte-for-byte intact;
//! * before the rename, the previous document is copied to `<path>.bak-<unix-seconds>`
//!   (mode 0600). The operator removes `.bak-*` files once the new document is verified
//!   (they hold key material).
//!
//! Nothing here prints a document; callers only ever see paths and `io::Error`s.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use zeroize::Zeroizing;

const MODE: u32 = 0o600;

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Opens `path` for an exclusive create with mode 0600.
fn create_exclusive(path: &Path) -> io::Result<fs::File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(MODE)
        .open(path)
}

fn write_all_synced(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut f = create_exclusive(path)?;
    f.write_all(content)?;
    f.sync_all()
}

/// Best-effort fsync of the directory holding `path` (makes the rename durable).
fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

/// Creates a NEW secret file (mode 0600); refuses to overwrite an existing one.
pub fn write_new(path: &Path, content: &str) -> io::Result<()> {
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing to overwrite an existing keyring",
        ));
    }
    let tmp = with_suffix(path, ".tmp");
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    write_all_synced(&tmp, content.as_bytes())?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent(path);
    Ok(())
}

/// Replaces an EXISTING secret file atomically, keeping a `.bak-<unix-seconds>` copy of
/// the previous document (mode 0600). Returns the backup path. On any failure the
/// original file is unchanged.
pub fn replace(path: &Path, content: &str) -> io::Result<PathBuf> {
    let previous = Zeroizing::new(fs::read(path)?);
    let backup = backup_path(path)?;
    write_all_synced(&backup, &previous)?;
    let tmp = with_suffix(path, ".tmp");
    // A stale temporary file of a crashed earlier run is never the live document.
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Err(e) = write_all_synced(&tmp, content.as_bytes()) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent(path);
    Ok(backup)
}

/// `<path>.bak-<unix-seconds>`; a second replacement within the same second gets a
/// numbered suffix instead of overwriting an earlier backup.
fn backup_path(path: &Path) -> io::Result<PathBuf> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = with_suffix(path, &format!(".bak-{seconds}"));
    if !base.exists() {
        return Ok(base);
    }
    for n in 1..1000 {
        let candidate = with_suffix(&base, &format!(".{n}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many keyring backups with the same timestamp",
    ))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("pr-secret-file-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".bak-"))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn new_files_are_exclusive_and_replacements_are_atomic_with_a_backup() {
        let dir = Dir::new();
        let path = dir.0.join("ck.json");
        write_new(&path, "one").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        assert_eq!(mode(&path), 0o600);
        assert!(
            write_new(&path, "two").is_err(),
            "never overwrites an existing keyring"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        let backup = replace(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "one");
        assert_eq!(mode(&backup), 0o600);
        assert_eq!(mode(&path), 0o600);
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ck.json.bak-"));
        assert!(!dir.0.join("ck.json.tmp").exists());
        // A second replacement in the same second gets its own backup.
        let backup2 = replace(&path, "three").unwrap();
        assert_ne!(backup, backup2);
        assert_eq!(fs::read_to_string(&backup2).unwrap(), "two");
        assert_eq!(backups(&dir.0).len(), 2);
    }

    #[test]
    fn a_failing_replacement_leaves_the_original_intact() {
        let dir = Dir::new();
        let path = dir.0.join("ck.json");
        write_new(&path, "live").unwrap();
        // Forced failure: the temporary path is occupied by a directory, so the new
        // document can neither be written nor renamed over the live file.
        fs::create_dir(dir.0.join("ck.json.tmp")).unwrap();
        let err = replace(&path, "broken").unwrap_err();
        assert!(!err.to_string().contains("broken"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "live");
        assert_eq!(mode(&path), 0o600);
        // A missing original is refused before anything is written.
        let missing = dir.0.join("none.json");
        assert!(replace(&missing, "x").is_err());
        assert!(!missing.exists());
        assert!(backups(&dir.0)
            .iter()
            .all(|b| !b.to_string_lossy().contains("none.json")));
    }
}
