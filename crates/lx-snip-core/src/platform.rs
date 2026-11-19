//! Módulo: `platform`
//! Objetivos: Definir traits e estruturas de abstração entre backends de captura e área de transferência (Wayland e X11).
//! Principais itens: `PlatformFuture`, `OutputGeometry`, `CapturedImage`, `PlatformError`, `CaptureBackend` e `ClipboardBackend`.
//! Última alteração: 2026-11-19

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

/// Futuro assíncrono em `Box` usado pelos traits de plataforma compatíveis com dispatch dinâmico.
pub type PlatformFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, PlatformError>> + Send + 'a>>;

/// Descreve a geometria lógica e escala de um monitor conectado.
/// Armazena: `name`, coordenadas `x`/`y`, dimensões `width`/`height` e `scale_factor` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputGeometry {
    /// Nome da saída no compositor ou RandR (ex.: `DP-1`, `eDP-1`).
    pub name: String,
    /// Origem X no espaço global de coordenadas do compositor.
    pub x: i32,
    /// Origem Y no espaço global de coordenadas do compositor.
    pub y: i32,
    /// Largura da saída em pixels.
    pub width: u32,
    /// Altura da saída em pixels.
    pub height: u32,
    /// Fator de escala configurado no monitor (ex.: `1.0`, `2.0`).
    pub scale_factor: f64,
}

/// Representa uma imagem capturada em memória no formato RGBA8 não pré-multiplicado.
/// Armazena: buffer `rgba`, `width`, `height` e lista `outputs` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedImage {
    /// Buffer de pixels RGBA8 (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// Largura da imagem em pixels.
    pub width: u32,
    /// Altura da imagem em pixels.
    pub height: u32,
    /// Geometrias dos monitores cobertos pela captura.
    pub outputs: Vec<OutputGeometry>,
}

/// Enumera os erros retornados pelos backends de captura e clipboard.
/// Armazena: variantes `Unsupported`, `CaptureFailed`, `ClipboardFailed` e `Cancelled` | Principais métodos: `Display`, `Error`.
#[derive(Debug, Error)]
pub enum PlatformError {
    /// Operação não suportada na sessão ou compositor ativo.
    #[error("unsupported platform operation: {0}")]
    Unsupported(String),
    /// Falha ao capturar a tela ou janela.
    #[error("capture failed: {0}")]
    CaptureFailed(String),
    /// Falha ao publicar ou manter dados na área de transferência.
    #[error("clipboard failed: {0}")]
    ClipboardFailed(String),
    /// Operação cancelada pelo usuário no portal ou overlay.
    #[error("operation cancelled by user")]
    Cancelled,
}

/// Define a interface padrão para backends de captura de tela (Wayland Portal ou X11).
/// Armazena: nenhum (trait) | Principais métodos: `supports_window_mode`, `capture_desktop`, `capture_window`.
pub trait CaptureBackend: Send + Sync {
    /// Indica se o backend suporta seleção direta de janela dentro do overlay congelado.
    /// Input: `&self` | Output: `true` no X11 ou `false` quando requer portal prévio.
    fn supports_window_mode(&self) -> bool;

    /// Captura a área de trabalho completa cobrindo todas as saídas ativas.
    /// Input: `&self` | Output: `PlatformFuture` resolvendo em `CapturedImage`.
    fn capture_desktop(&self) -> PlatformFuture<'_, CapturedImage>;

    /// Captura uma janela específica por seletor do portal ou lista do servidor X11.
    /// Input: `&self` | Output: `PlatformFuture` resolvendo em `CapturedImage`.
    fn capture_window(&self) -> PlatformFuture<'_, CapturedImage>;
}

/// Define a interface para publicação persistente de PNG na área de transferência.
/// Armazena: nenhum (trait) | Principais métodos: `copy_png_and_hold`.
pub trait ClipboardBackend: Send + Sync {
    /// Publica um PNG na área de transferência e mantém a posse até ser substituído.
    /// Input: `&self` e `png_bytes` codificados | Output: `PlatformFuture` indicando conclusão ou erro.
    fn copy_png_and_hold(&self, png_bytes: Vec<u8>) -> PlatformFuture<'_, ()>;
}
