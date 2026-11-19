//! Módulo: `config`
//! Objetivos: Definir o esquema de configuração da aplicação e garantir leitura e gravação atômica em TOML.
//! Principais itens: `CaptureMode`, `AppConfig`, `ConfigError`, `default_config_path`, `load_or_default` e `save_atomic`.
//! Última alteração: 2026-11-19

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Limite padrão de capturas PNG mantidas no histórico local.
pub const DEFAULT_HISTORY_MAX_ITEMS: usize = 50;

/// Enumera os modos de captura e gravação suportados pela aplicação.
/// Armazena: variantes `Rect`, `Freeform`, `Window`, `Fullscreen` e `Record` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    /// Seleção retangular para captura de imagem.
    #[default]
    Rect,
    /// Seleção à mão livre (laço).
    Freeform,
    /// Captura de uma janela individual.
    Window,
    /// Captura de toda a área de trabalho em tela cheia.
    Fullscreen,
    /// Modo de gravação de tela (vídeo e áudio).
    Record,
}

/// Representa a configuração persistente do usuário armazenada em `config.toml`.
/// Armazena: `default_mode`, `default_delay_secs`, `auto_copy` e `history_max_items` | Principais métodos: `Default::default`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Modo padrão usado quando o app abre sem `--mode`.
    pub default_mode: CaptureMode,
    /// Atraso padrão em segundos antes da captura (`0`, `3`, `5` ou `10`).
    pub default_delay_secs: u8,
    /// Define se a imagem exportada é copiada automaticamente para a área de transferência.
    pub auto_copy: bool,
    /// Quantidade máxima de arquivos PNG mantidos no histórico local.
    pub history_max_items: usize,
}

impl Default for AppConfig {
    /// Constrói a configuração padrão inicial da aplicação.
    /// Input: nenhum | Output: instância padrão de `AppConfig`.
    fn default() -> Self {
        Self {
            default_mode: CaptureMode::Rect,
            default_delay_secs: 0,
            auto_copy: true,
            history_max_items: DEFAULT_HISTORY_MAX_ITEMS,
        }
    }
}

/// Enumera as falhas possíveis ao localizar, ler, validar ou salvar `AppConfig`.
/// Armazena: variantes `NoConfigDir`, `Io`, `Parse` e `Serialize` | Principais métodos: `Display`, `Error`.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Falha ao resolver o diretório de configuração XDG.
    #[error("unable to determine XDG configuration directory")]
    NoConfigDir,
    /// Erro de entrada/saída ao acessar o arquivo de configuração.
    #[error("config I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Erro ao interpretar o arquivo TOML.
    #[error("failed to parse config TOML: {0}")]
    Parse(#[from] toml::de::Error),
    /// Erro ao serializar a estrutura em TOML.
    #[error("failed to serialize config TOML: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Resolve o caminho padrão do arquivo de configuração XDG da aplicação.
/// Input: nenhum | Output: `Result<PathBuf, ConfigError>` apontando para `config.toml`.
pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from("io.github", "lx_snippingtool", "lx-snippingtool")
        .ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Carrega a configuração do caminho XDG padrão ou devolve os valores padrão se ausente.
/// Input: nenhum | Output: `Result<AppConfig, ConfigError>` com a configuração carregada.
pub fn load_or_default() -> Result<AppConfig, ConfigError> {
    let path = default_config_path()?;
    load_or_default_from(&path)
}

/// Lê `AppConfig` de um caminho específico, retornando `AppConfig::default()` se o arquivo não existir.
/// Input: `path` do arquivo TOML | Output: `Result<AppConfig, ConfigError>` carregado ou padrão.
pub fn load_or_default_from(path: &Path) -> Result<AppConfig, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Salva `AppConfig` de forma atômica no caminho XDG padrão.
/// Input: referência a `AppConfig` | Output: `Result<(), ConfigError>` indicando sucesso ou erro.
pub fn save_atomic(config: &AppConfig) -> Result<(), ConfigError> {
    let path = default_config_path()?;
    save_atomic_to(config, &path)
}

/// Escreve `AppConfig` de forma atômica em `path` usando arquivo temporário e `rename`.
/// Input: referência a `AppConfig` e `path` de destino | Output: `Result<(), ConfigError>` da operação.
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
