//! Módulo: `storage`
//! Objetivos: Gerenciar o armazenamento local limitado de capturas PNG e tokens do portal `ScreenCast` com permissões restritas (`0700`/`0600`).
//! Principais itens: `SnipEntry`, `StorageError`, `SnipStore` e `TokenStore`.
//! Última alteração: 2026-11-19

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

/// Representa os metadados de uma captura PNG armazenada no histórico local.
/// Armazena: `path`, `created_at` e `byte_size` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnipEntry {
    /// Caminho completo do arquivo PNG.
    pub path: PathBuf,
    /// Data de modificação usada para ordenar e podar o histórico.
    pub created_at: SystemTime,
    /// Tamanho do arquivo em bytes.
    pub byte_size: u64,
}

/// Enumera erros retornados pelas operações de armazenamento de `SnipStore` e `TokenStore`.
/// Armazena: variantes `NoDataDir` e `Io` | Principais métodos: `Display`, `Error`.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Falha ao localizar o diretório XDG de dados da aplicação.
    #[error("unable to determine XDG data directory")]
    NoDataDir,
    /// Erro de sistema de arquivos durante leitura ou escrita.
    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Repositório local limitado de capturas PNG com poda automática dos itens mais antigos.
/// Armazena: diretório base `dir` e limite `max_items` | Principais métodos: `new_default`, `save_png`, `list`, `prune`, `delete`.
#[derive(Debug, Clone)]
pub struct SnipStore {
    dir: PathBuf,
    max_items: usize,
}

impl SnipStore {
    /// Cria um `SnipStore` no diretório padrão XDG (`$XDG_DATA_HOME/lx-snippingtool/snips/`).
    /// Input: limite `max_items` | Output: `Result<SnipStore, StorageError>`.
    pub fn new_default(max_items: usize) -> Result<Self, StorageError> {
        let dirs = ProjectDirs::from("io.github", "lx_snippingtool", "lx-snippingtool")
            .ok_or(StorageError::NoDataDir)?;
        Ok(Self::new(dirs.data_dir().join("snips"), max_items))
    }

    /// Cria um `SnipStore` apontando para um diretório explícito.
    /// Input: diretório `dir` e limite `max_items` | Output: instância de `SnipStore`.
    pub fn new(dir: impl Into<PathBuf>, max_items: usize) -> Self {
        Self {
            dir: dir.into(),
            max_items,
        }
    }

    /// Retorna o caminho do diretório onde os PNGs são armazenados.
    /// Input: `&self` | Output: referência `&Path` ao diretório.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Retorna o número máximo de arquivos retidos pelo repositório.
    /// Input: `&self` | Output: valor `usize` configurado.
    pub fn max_items(&self) -> usize {
        self.max_items
    }

    /// Garante que o diretório de capturas exista com permissão restrita `0700`.
    /// Input: `&self` | Output: `Result<(), StorageError>`.
    pub fn ensure_dir(&self) -> Result<(), StorageError> {
        fs::create_dir_all(&self.dir)?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    /// Salva bytes PNG com permissão `0600` e remove arquivos excedentes além de `max_items`.
    /// Input: `&self` e slice `png_bytes` | Output: `Result<PathBuf, StorageError>` com o caminho criado.
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

    /// Lista todos os arquivos `.png` ordenados do mais recente ao mais antigo.
    /// Input: `&self` | Output: `Result<Vec<SnipEntry>, StorageError>`.
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

    /// Remove capturas antigas que excedem `max_items` e informa quantas foram apagadas.
    /// Input: `&self` | Output: `Result<usize, StorageError>` com total removido.
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

    /// Exclui do disco um arquivo específico pertencente ao histórico.
    /// Input: `&self` e `path` do arquivo | Output: `Result<(), StorageError>`.
    pub fn delete(&self, path: &Path) -> Result<(), StorageError> {
        fs::remove_file(path)?;
        Ok(())
    }
}

/// Armazena o token persistente do portal `ScreenCast` (`restore_token`) em `tokens.json`.
/// Armazena: `schema_version` e `screencast_restore_token` | Principais métodos: `default_path`, `load_default`, `save_default`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenStore {
    /// Versão do esquema para compatibilidade futura.
    pub schema_version: u32,
    /// Token salvo do portal `ScreenCast` (`persist_mode = 2`).
    pub screencast_restore_token: Option<String>,
}

impl Default for TokenStore {
    /// Constrói um `TokenStore` vazio na versão de esquema 1.
    /// Input: nenhum | Output: `TokenStore` inicial padrão.
    fn default() -> Self {
        Self {
            schema_version: 1,
            screencast_restore_token: None,
        }
    }
}

impl TokenStore {
    /// Resolve o caminho padrão de `tokens.json` no diretório XDG de dados.
    /// Input: nenhum | Output: `Result<PathBuf, StorageError>`.
    pub fn default_path() -> Result<PathBuf, StorageError> {
        let proj = ProjectDirs::from("", "", "lx-snippingtool").ok_or(StorageError::NoDataDir)?;
        Ok(proj.data_dir().join("tokens.json"))
    }

    /// Carrega `tokens.json` do caminho padrão ou devolve `Default` se ausente/inválido.
    /// Input: nenhum | Output: instância carregada ou padrão de `TokenStore`.
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

    /// Persiste `tokens.json` atomicamente com permissões `0700` no diretório e `0600` no arquivo.
    /// Input: `&self` | Output: `Result<(), StorageError>`.
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
