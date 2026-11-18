//! Application configuration schema and atomic persistence.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Default maximum number of snips retained in local history.
pub const DEFAULT_HISTORY_MAX_ITEMS: usize = 50;

/// Supported screen capture modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    /// Rectangular region selection.
    #[default]
    Rect,
    /// Freeform lasso selection.
    Freeform,
    /// Single window capture.
    Window,
    /// Fullscreen (all outputs) capture.
    Fullscreen,
    /// Rectangular region screen recording (Phase 2).
    Record,
}

/// Persistent user configuration stored at `$XDG_CONFIG_HOME/lx-snippingtool/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Default capture mode when launched without `--mode`.
    pub default_mode: CaptureMode,
    /// Default pre-capture delay in seconds (e.g. 0, 3, 5, 10).
    pub default_delay_secs: u8,
    /// Whether completed snips are automatically copied to the clipboard.
    pub auto_copy: bool,
    /// Maximum number of PNG files retained in the local snip history directory.
    pub history_max_items: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_mode: CaptureMode::Rect,
            default_delay_secs: 0,
            auto_copy: true,
            history_max_items: DEFAULT_HISTORY_MAX_ITEMS,
        }
    }
}

/// Errors that can occur while loading or saving [`AppConfig`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to resolve the XDG configuration directory.
    #[error("unable to determine XDG configuration directory")]
    NoConfigDir,
    /// Filesystem I/O failure.
    #[error("config I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// TOML deserialization failure.
    #[error("failed to parse config TOML: {0}")]
    Parse(#[from] toml::de::Error),
    /// TOML serialization failure.
    #[error("failed to serialize config TOML: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Resolves the default configuration file path (`$XDG_CONFIG_HOME/lx-snippingtool/config.toml`).
pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from("io.github", "lx_snippingtool", "lx-snippingtool")
        .ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Loads [`AppConfig`] from the default XDG config path, returning [`AppConfig::default()`]
/// if the file does not exist yet.
pub fn load_or_default() -> Result<AppConfig, ConfigError> {
    let path = default_config_path()?;
    load_or_default_from(&path)
}

/// Loads [`AppConfig`] from `path`, returning [`AppConfig::default()`] if `path` does not exist.
pub fn load_or_default_from(path: &Path) -> Result<AppConfig, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Atomically writes `config` to the default XDG config path.
pub fn save_atomic(config: &AppConfig) -> Result<(), ConfigError> {
    let path = default_config_path()?;
    save_atomic_to(config, &path)
}

/// Atomically writes `config` to `path` via a sibling temporary file and rename.
pub fn save_atomic_to(config: &AppConfig, path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let serialized = toml::to_string_pretty(config)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_name = format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config.toml"),
        Uuid::new_v4()
    );
    let tmp_path = parent.join(tmp_name);

    let write_res = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(serialized.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_res.map_err(ConfigError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lx_snip_cfg_test_{}_{}", Uuid::new_v4(), name))
    }

    #[test]
    fn defaults_match_spec() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.default_mode, CaptureMode::Rect);
        assert_eq!(cfg.default_delay_secs, 0);
        assert!(cfg.auto_copy);
        assert_eq!(cfg.history_max_items, 50);
    }

    #[test]
    fn missing_file_returns_default() {
        let path = temp_path("missing.toml");
        let cfg = load_or_default_from(&path).expect("missing file should return default");
        assert_eq!(cfg, AppConfig::default());
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let path = temp_path("partial.toml");
        fs::write(
            &path,
            "default_mode = \"freeform\"\ndefault_delay_secs = 5\n",
        )
        .unwrap();
        let cfg = load_or_default_from(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(cfg.default_mode, CaptureMode::Freeform);
        assert_eq!(cfg.default_delay_secs, 5);
        assert!(cfg.auto_copy);
        assert_eq!(cfg.history_max_items, 50);
    }

    #[test]
    fn save_atomic_round_trip() {
        let dir = temp_path("dir");
        let path = dir.join("config.toml");
        let cfg = AppConfig {
            default_mode: CaptureMode::Window,
            default_delay_secs: 10,
            auto_copy: false,
            history_max_items: 25,
        };

        save_atomic_to(&cfg, &path).unwrap();
        let loaded = load_or_default_from(&path).unwrap();
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(loaded, cfg);
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let path = temp_path("invalid.toml");
        fs::write(&path, "default_mode = [invalid").unwrap();
        let err = load_or_default_from(&path).unwrap_err();
        let _ = fs::remove_file(&path);

        assert!(matches!(err, ConfigError::Parse(_)));
    }
}
