//! Bounded local PNG snip history store with strict directory/file permissions.

use std::cmp::Ordering;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Metadata for a stored PNG snip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnipEntry {
    /// Full path to the PNG file.
    pub path: PathBuf,
    /// Last modification timestamp used for history ordering and pruning.
    pub created_at: SystemTime,
    /// File size in bytes.
    pub byte_size: u64,
}

/// Errors that can occur in [`SnipStore`].
#[derive(Debug, Error)]
pub enum StorageError {
    /// Failed to resolve the XDG data directory.
    #[error("unable to determine XDG data directory")]
    NoDataDir,
    /// Filesystem I/O failure.
    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Bounded local PNG history store under `$XDG_DATA_HOME/lx-snippingtool/snips/`.
#[derive(Debug, Clone)]
pub struct SnipStore {
    dir: PathBuf,
    max_items: usize,
}

impl SnipStore {
    /// Creates a [`SnipStore`] rooted at `$XDG_DATA_HOME/lx-snippingtool/snips/`.
    pub fn new_default(max_items: usize) -> Result<Self, StorageError> {
        let dirs = ProjectDirs::from("io.github", "lx_snippingtool", "lx-snippingtool")
            .ok_or(StorageError::NoDataDir)?;
        Ok(Self::new(dirs.data_dir().join("snips"), max_items))
    }

    /// Creates a [`SnipStore`] at an explicit directory path.
    pub fn new(dir: impl Into<PathBuf>, max_items: usize) -> Self {
        Self {
            dir: dir.into(),
            max_items,
        }
    }

    /// Returns the snips directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the maximum number of retained snips.
    pub fn max_items(&self) -> usize {
        self.max_items
    }

    /// Ensures the snips directory exists and enforces `0700` permissions.
    pub fn ensure_dir(&self) -> Result<(), StorageError> {
        fs::create_dir_all(&self.dir)?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    /// Saves `png_bytes` as `<unix_ms>_<uuid>.png` with `0600` permissions and prunes
    /// oldest entries if the store exceeds `max_items`.
    pub fn save_png(&self, png_bytes: &[u8]) -> Result<PathBuf, StorageError> {
        self.ensure_dir()?;

        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let filename = format!("{unix_ms}_{}.png", Uuid::new_v4());
        let dest_path = self.dir.join(filename);

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&dest_path)?;
        file.write_all(png_bytes)?;
        file.sync_all()?;
        // Explicitly set 0600 in case process umask masked bits or filesystem adjusted defaults.
        fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o600))?;

        self.prune()?;
        Ok(dest_path)
    }

    /// Lists stored `.png` snips sorted newest-first by mtime (with filename tiebreaker).
    pub fn list(&self) -> Result<Vec<SnipEntry>, StorageError> {
        let read_dir = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(StorageError::Io(err)),
        };

        let mut entries = Vec::new();
        for item in read_dir {
            let entry = item?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("png") {
                continue;
            }
            let meta = entry.metadata()?;
            if !meta.is_file() {
                continue;
            }
            let created_at = meta.modified().unwrap_or(UNIX_EPOCH);
            entries.push(SnipEntry {
                path,
                created_at,
                byte_size: meta.len(),
            });
        }

        entries.sort_by(|a, b| match b.created_at.cmp(&a.created_at) {
            Ordering::Equal => b.path.file_name().cmp(&a.path.file_name()),
            other => other,
        });

        Ok(entries)
    }

    /// Removes oldest snips beyond `max_items`, returning the number of files deleted.
    pub fn prune(&self) -> Result<usize, StorageError> {
        let entries = self.list()?;
        if entries.len() <= self.max_items {
            return Ok(0);
        }

        let mut removed = 0;
        for entry in &entries[self.max_items..] {
            match fs::remove_file(&entry.path) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(StorageError::Io(err)),
            }
        }
        Ok(removed)
    }

    /// Deletes a specific snip file from the store.
    pub fn delete(&self, path: &Path) -> Result<(), StorageError> {
        fs::remove_file(path)?;
        Ok(())
    }
}

/// Persistent `xdg-desktop-portal` `ScreenCast` `restore_token` store (`tokens.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenStore {
    /// Schema version for forward/backward compatibility.
    pub schema_version: u32,
    /// Saved `ScreenCast` `restore_token` (`persist_mode = 2`).
    pub screencast_restore_token: Option<String>,
}

impl Default for TokenStore {
    fn default() -> Self {
        Self {
            schema_version: 1,
            screencast_restore_token: None,
        }
    }
}

impl TokenStore {
    /// Default `tokens.json` path under `$XDG_DATA_HOME/lx-snippingtool/tokens.json`.
    pub fn default_path() -> Result<PathBuf, StorageError> {
        let proj = ProjectDirs::from("", "", "lx-snippingtool").ok_or(StorageError::NoDataDir)?;
        Ok(proj.data_dir().join("tokens.json"))
    }

    /// Loads `tokens.json` from the default path (returning `Default` if missing or corrupt).
    pub fn load_default() -> Self {
        let Ok(path) = Self::default_path() else {
            return Self::default();
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(store) if store.schema_version == 1 => store,
            _ => Self::default(),
        }
    }

    /// Saves `tokens.json` atomically with `0700` dir and `0600` file permissions.
    pub fn save_default(&self) -> Result<(), StorageError> {
        let path = Self::default_path()?;
        let parent = path.parent().ok_or(StorageError::NoDataDir)?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;

        let tmp = parent.join(format!("tokens.json.tmp.{}", Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}
