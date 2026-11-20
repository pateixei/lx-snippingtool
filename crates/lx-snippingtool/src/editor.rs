//! Módulo: `editor`
//! Objetivos: Implementar a janela do editor de anotações de capturas de tela no estilo Windows 11 Snipping Tool (temas Claro/Escuro), suportando desenho livre, estilos de linha (sólida, pontilhada, tracejada), marca-texto, spray, borracha, curvas Catmull-Rom/Bézier, formas geométricas (retângulo, esfera/elipse, linha, seta, seta dupla), tarja de vidro fosco fumê (desfoque forte + camada escura 50% + dispersão estocástica), recorte, cores personalizadas via seletor do sistema (`GTK4`/`zenity` no Linux; `ChooseColorW` no Windows), desfazer/refazer, cópia e exportação PNG/JPEG (`ashpd` no Linux; `GetSaveFileNameW` no Windows).
//! Principais itens: `UiTheme`, `EditorTool`, `EditorMessage`, `EditorAction`, `EditorState`, `EditorCanvas`, `pick_color_via_system`, `save_as_via_portal`
//! Última alteração: 2026-11-20

use std::path::PathBuf;

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, LineDash, LineJoin, Path, Stroke};
use iced::widget::{button, container, row, text, Column};
use iced::{event, Alignment, Color, Element, Length, Point as IcedPoint, Rectangle, Size};
use lx_snip_core::export::{render_and_export, ExportFormat, DEFAULT_JPEG_QUALITY};
use lx_snip_core::scene::{
    catmull_rom_spline_points, spray_stamp_points, AnnotationOp, AnnotationScene,
    Color as SceneColor, LineStyle, Point, Rect, ShapeKind,
};

use crate::icons;

/// Quantidade máxima de cores personalizadas que podem ser adicionadas pelo seletor do sistema.
pub const MAX_CUSTOM_COLORS: usize = 3;

/// Esquema visual Fluent do editor alternando entre modo claro e escuro.
/// Armazena: variantes `Light` e `Dark` | Principais métodos: `from_dark`, `palette`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTheme {
    /// Tema claro padrão estilo Windows 11 Fluent Light.
    Light,
    /// Tema escuro estilo Windows 11 Fluent Dark.
    Dark,
}

/// Paleta concreta de cores de fundo, borda, texto e realce usada pelos widgets do editor.
/// Armazena: `header_bg`, `workspace_bg`, `card_bg`, `card_hover`, `card_border`, `pill_bg`, `text`, `text_muted`, `accent`, `accent_hover`, `active_bg` | Principais métodos: nenhum (estrutura de paleta)
#[derive(Debug, Clone, Copy)]
struct ThemePalette {
    header_bg: Color,
    workspace_bg: Color,
    card_bg: Color,
    card_hover: Color,
    card_border: Color,
    pill_bg: Color,
    text: Color,
    text_muted: Color,
    accent: Color,
    accent_hover: Color,
    active_bg: Color,
}

impl UiTheme {
    /// Converte a preferência booleana de modo escuro do sistema em `UiTheme`.
    /// Input: `dark` (`bool`) | Output: `UiTheme::Dark` ou `UiTheme::Light`
    pub const fn from_dark(dark: bool) -> Self {
        if dark {
            Self::Dark
        } else {
            Self::Light
        }
    }

    /// Retorna a paleta de cores `ThemePalette` correspondente à variante de tema ativa.
    /// Input: `self` | Output: instância de `ThemePalette`
    fn palette(self) -> ThemePalette {
        match self {
            UiTheme::Light => ThemePalette {
                header_bg: Color::from_rgb8(243, 243, 243),
                workspace_bg: Color::from_rgb8(250, 250, 250),
                card_bg: Color::from_rgb8(255, 255, 255),
                card_hover: Color::from_rgb8(246, 246, 246),
                card_border: Color::from_rgb8(222, 222, 226),
                pill_bg: Color::from_rgb8(236, 236, 240),
                text: Color::from_rgb8(26, 26, 28),
                text_muted: Color::from_rgb8(105, 105, 112),
                accent: Color::from_rgb8(0, 103, 192),
                accent_hover: Color::from_rgb8(16, 118, 210),
                active_bg: Color::from_rgb8(230, 240, 250),
            },
            UiTheme::Dark => ThemePalette {
                header_bg: Color::from_rgb8(32, 32, 34),
                workspace_bg: Color::from_rgb8(24, 24, 26),
                card_bg: Color::from_rgb8(45, 45, 48),
                card_hover: Color::from_rgb8(56, 56, 60),
                card_border: Color::from_rgb8(64, 64, 70),
                pill_bg: Color::from_rgb8(40, 40, 44),
                text: Color::from_rgb8(244, 244, 246),
                text_muted: Color::from_rgb8(158, 158, 166),
                accent: Color::from_rgb8(76, 194, 255),
                accent_hover: Color::from_rgb8(98, 204, 255),
                active_bg: Color::from_rgb8(42, 58, 72),
            },
        }
    }
}

/// Ferramenta de desenho ou edição atualmente selecionada na barra do editor.
/// Armazena: variantes `Pen`, `Highlighter`, `Spray`, `Eraser`, `CurvedLine`, `Shape`, `Redact` e `Crop` | Principais métodos: nenhum (enumeração de ferramentas)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTool {
    /// Freehand opaque pen.
    Pen,
    /// Translucent highlighter.
    Highlighter,
    /// Airbrush / spray brush dispersing droplets around the stroke.
    Spray,
    /// Stroke-level eraser.
    Eraser,
    /// Smooth Catmull-Rom / cubic Bézier interpolated curved line through waypoints.
    CurvedLine,
    /// Geometric shape tool.
    Shape(ShapeKind),
    /// Destructive solid-black redaction box with soft gradient border.
    Redact,
    /// Crop rectangle tool.
    Crop,
}

/// Mensagens de interação e eventos assíncronos processados pelo editor de anotações.
/// Armazena: eventos de seleção de ferramenta/estilo de linha/cor/espessura, seletor de cores do sistema, ponteiro, desfazer/refazer, recorte, exportação, histórico, tema e controles de janela | Principais métodos: nenhum (tipo de mensagem)
#[derive(Debug, Clone)]
pub enum EditorMessage {
    /// Switch active annotation tool.
    SelectTool(EditorTool),
    /// Switch active line style (solid, dotted, dashed).
    SelectLineStyle(LineStyle),
    /// Switch active stroke color.
    SelectColor(SceneColor),
    /// Open the standard system color dialog to add or update a custom color slot (`0..3`).
    RequestCustomColor(usize),
    /// Result returned from the system color chooser dialog for slot `usize`.
    CustomColorPicked(usize, Option<SceneColor>),
    /// Set stroke width in pixels.
    SetStrokeWidth(f32),
    /// Pointer pressed in image-pixel coordinates.
    PointerDown(Point),
    /// Pointer moved in image-pixel coordinates.
    PointerMove(Point),
    /// Pointer hovered over canvas without pressing mouse button (elastic spline preview).
    PointerHover(Point),
    /// Pointer released in image-pixel coordinates.
    PointerUp(Point),
    /// Finalize active multi-click `CurvedLine` waypoints into the scene.
    FinishCurve,
    /// Cancel active un-committed `CurvedLine` waypoints.
    CancelCurve,
    /// Undo last scene mutation.
    Undo,
    /// Redo previously undone scene mutation.
    Redo,
    /// Clear active crop rectangle.
    ResetCrop,
    /// Render, save to snip history, copy to clipboard, and close UI.
    CopyAndExit,
    /// Open native portal Save As dialog and export image.
    SaveAs,
    /// Result of Save As portal + file write.
    SaveCompleted(Result<Option<PathBuf>, String>),
    /// Trigger a new capture from the editor.
    NewCapture,
    /// Open Snip History browser from the editor.
    OpenHistory,
    /// Toggle between Windows 11 Light and Dark themes.
    ToggleTheme,
}

/// Ação externa solicitada ao controlador principal por `EditorState::update`.
/// Armazena: variantes `CopyAndExit`, `RunSaveAs`, `PickCustomColor`, `NewCapture` e `OpenHistory` | Principais métodos: nenhum (enumeração de saída)
#[derive(Debug, Clone)]
pub enum EditorAction {
    /// Exported PNG bytes ready to be saved to `SnipStore` and handed to `ClipboardBackend`.
    CopyAndExit(Vec<u8>),
    /// Trigger an async Save As dialog for the given exported image snapshot.
    RunSaveAs {
        /// Base straight RGBA8 pixels.
        base_rgba: Vec<u8>,
        /// Base width in pixels.
        width: u32,
        /// Base height in pixels.
        height: u32,
        /// Cloned scene to rasterize.
        scene: AnnotationScene,
    },
    /// Open the standard system color chooser dialog for the requested custom slot (`0..3`).
    PickCustomColor {
        /// Custom color slot index (`0..3`).
        slot: usize,
        /// Initial color to pre-fill in the color dialog.
        initial: SceneColor,
    },
    /// Start a new screen capture.
    NewCapture,
    /// Open Snip History window.
    OpenHistory,
}

/// Estado completo da janela do editor de anotações, cena vetorial e tema ativo.
/// Armazena: `base_rgba`, `width`, `height`, `bg_handle`, `scene`, `active_tool`, `line_style`, `active_color`, `custom_colors`, `stroke_width`, `in_progress_points`, `curve_waypoints`, `hover_point`, `curve_drag_mode`, `drag_start`, `drag_current`, `status_text`, `theme` | Principais métodos: `new`, `from_png_file`, `with_theme`, `visible_crop_bounds`, `update`, `view`
#[derive(Debug, Clone)]
pub struct EditorState {
    /// Base straight RGBA8 image buffer.
    pub base_rgba: Vec<u8>,
    /// Base image width in pixels.
    pub width: u32,
    /// Base image height in pixels.
    pub height: u32,
    /// Cached `iced::widget::image::Handle` for fast canvas rendering.
    pub bg_handle: iced::widget::image::Handle,
    /// Retained-mode annotation scene.
    pub scene: AnnotationScene,
    /// Active tool.
    pub active_tool: EditorTool,
    /// Active line style (solid, dotted, dashed) for pen and geometric shapes.
    pub line_style: LineStyle,
    /// Active stroke color.
    pub active_color: SceneColor,
    /// Up to 3 user-defined custom colors picked via the system color chooser.
    pub custom_colors: Vec<SceneColor>,
    /// Active stroke width in image pixels.
    pub stroke_width: f32,
    /// Active freehand stroke points.
    pub in_progress_points: Vec<Point>,
    /// Active multi-click waypoints for `EditorTool::CurvedLine`.
    pub curve_waypoints: Vec<Point>,
    /// Last hover position in image pixels for elastic `CurvedLine` preview.
    pub hover_point: Option<Point>,
    /// Tracks whether current `CurvedLine` pointer interaction exceeded drag threshold.
    pub curve_drag_mode: bool,
    /// Active drag start for shape/redact/crop.
    pub drag_start: Option<Point>,
    /// Active drag end for shape/redact/crop.
    pub drag_current: Option<Point>,
    /// Status banner message (e.g. saved path or error).
    pub status_text: Option<String>,
    /// Active Windows 11 Fluent theme (Light or Dark).
    pub theme: UiTheme,
}

impl EditorState {
    /// Inicializa um novo estado do editor a partir de um buffer de pixels RGBA8 sem compressão.
    /// Input: `base_rgba` (`Vec<u8>`), `width` (`u32`), `height` (`u32`) | Output: instância de `EditorState`
    pub fn new(base_rgba: Vec<u8>, width: u32, height: u32) -> Self {
        let bg_handle = iced::widget::image::Handle::from_rgba(width, height, base_rgba.clone());
        Self {
            base_rgba,
            width,
            height,
            bg_handle,
            scene: AnnotationScene::new(width, height),
            active_tool: EditorTool::Pen,
            line_style: LineStyle::Solid,
            active_color: SceneColor::RED,
            custom_colors: Vec::new(),
            stroke_width: 4.0,
            in_progress_points: Vec::new(),
            curve_waypoints: Vec::new(),
            hover_point: None,
            curve_drag_mode: false,
            drag_start: None,
            drag_current: None,
            status_text: None,
            theme: UiTheme::Light,
        }
    }

    /// Carrega uma imagem PNG do disco e cria um `EditorState` pronto para anotação.
    /// Input: `path` (`&std::path::Path`) | Output: `Result<EditorState, String>`
    pub fn from_png_file(path: &std::path::Path) -> Result<Self, String> {
        let dyn_img =
            image::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        let rgba_img = dyn_img.to_rgba8();
        let (w, h) = rgba_img.dimensions();
        Ok(Self::new(rgba_img.into_raw(), w, h))
    }

    /// Define o tema visual inicial (`UiTheme`) do editor e retorna o estado configurado.
    /// Input: `mut self`, `theme` (`UiTheme`) | Output: `Self` com `self.theme` atualizado
    pub const fn with_theme(mut self, theme: UiTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Converte um retângulo opcional de recorte em limites inteiros visíveis dentro da imagem base.
    /// Input: `&self` e `crop` (`Option<Rect>`) | Output: tupla `(x, y, largura, altura, ativo)`.
    fn resolve_viewport_bounds(&self, crop: Option<Rect>) -> (u32, u32, u32, u32, bool) {
        let Some(raw) = crop else {
            return (0, 0, self.width, self.height, false);
        };
        let clamped = raw.clamp(self.width as f32, self.height as f32);
        let x0 = (clamped.x.floor() as u32).min(self.width);
        let y0 = (clamped.y.floor() as u32).min(self.height);
        let x1 = ((clamped.x + clamped.width).ceil() as u32).min(self.width);
        let y1 = ((clamped.y + clamped.height).ceil() as u32).min(self.height);
        let cw = x1.saturating_sub(x0);
        let ch = y1.saturating_sub(y0);
        if cw == 0 || ch == 0 {
            return (0, 0, self.width, self.height, false);
        }
        (x0, y0, cw, ch, true)
    }

    /// Retorna os limites inteiros do recorte ativo para exibição imediata no canvas do editor.
    /// Input: `&self` | Output: tupla `(x, y, largura, altura, ativo)`.
    pub fn visible_crop_bounds(&self) -> (u32, u32, u32, u32, bool) {
        self.resolve_viewport_bounds(self.scene.crop_rect)
    }

    /// Atualiza `bg_handle` para exibir apenas a sub-região visível definida pelo recorte ativo.
    /// Input: `&mut self` | Output: `()` atualizando `self.bg_handle`.
    fn sync_bg_handle(&mut self) {
        let (vx, vy, vw, vh, is_cropped) = self.visible_crop_bounds();
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if !is_cropped || self.width == 0 || self.height == 0 || self.base_rgba.len() != expected {
            self.bg_handle = iced::widget::image::Handle::from_rgba(
                self.width,
                self.height,
                self.base_rgba.clone(),
            );
            return;
        }
        let mut cropped = Vec::with_capacity((vw as usize) * (vh as usize) * 4);
        let src_stride = (self.width as usize) * 4;
        let row_bytes = (vw as usize) * 4;
        let start_col = (vx as usize) * 4;
        for row in vy..(vy + vh) {
            let offset = (row as usize) * src_stride + start_col;
            cropped.extend_from_slice(&self.base_rgba[offset..offset + row_bytes]);
        }
        self.bg_handle = iced::widget::image::Handle::from_rgba(vw, vh, cropped);
    }

    /// Confirma na cena os pontos de passagem pendentes de `CurvedLine`, caso haja pelo menos 2 pontos.
    /// Input: `&mut self` | Output: `()` atualizando `self.scene`.
    fn commit_curve_waypoints(&mut self) {
        self.hover_point = None;
        self.curve_drag_mode = false;
        let pts = std::mem::take(&mut self.curve_waypoints);
        if pts.len() >= 2 {
            self.scene.push_op(AnnotationOp::CurvedLine {
                points: pts,
                color: self.active_color,
                width: self.stroke_width,
                style: self.line_style,
            });
        }
    }

    /// Processa uma mensagem da interface do editor atualizando ferramentas/cena e emitindo ação externa opcional.
    /// Input: `&mut self`, `msg` (`EditorMessage`) | Output: `Option<EditorAction>`
    pub fn update(&mut self, msg: EditorMessage) -> Option<EditorAction> {
        match msg {
            EditorMessage::SelectTool(tool) => {
                if self.active_tool != tool {
                    self.commit_curve_waypoints();
                }
                self.active_tool = tool;
                self.in_progress_points.clear();
                self.drag_start = None;
                self.drag_current = None;
                None
            }
            EditorMessage::SelectLineStyle(style) => {
                self.line_style = style;
                None
            }
            EditorMessage::SelectColor(color) => {
                self.active_color = color;
                None
            }
            EditorMessage::RequestCustomColor(slot) => {
                if slot >= MAX_CUSTOM_COLORS {
                    return None;
                }
                let initial = self
                    .custom_colors
                    .get(slot)
                    .copied()
                    .unwrap_or(self.active_color);
                Some(EditorAction::PickCustomColor { slot, initial })
            }
            EditorMessage::CustomColorPicked(slot, Some(color)) => {
                if slot < self.custom_colors.len() {
                    self.custom_colors[slot] = color;
                } else if self.custom_colors.len() < MAX_CUSTOM_COLORS {
                    self.custom_colors.push(color);
                }
                self.active_color = color;
                None
            }
            EditorMessage::CustomColorPicked(_, None) => None,
            EditorMessage::SetStrokeWidth(w) => {
                self.stroke_width = w.clamp(1.0, 48.0);
                None
            }
            EditorMessage::PointerDown(pt) => {
                match self.active_tool {
                    EditorTool::Pen | EditorTool::Highlighter | EditorTool::Spray => {
                        self.in_progress_points.clear();
                        self.in_progress_points.push(pt);
                    }
                    EditorTool::CurvedLine => {
                        self.in_progress_points.clear();
                        self.in_progress_points.push(pt);
                        self.curve_drag_mode = false;
                        self.drag_start = Some(pt);
                        self.drag_current = Some(pt);
                    }
                    EditorTool::Eraser => {
                        let radius = (self.stroke_width * 2.0).max(10.0);
                        self.scene.erase_at(pt, radius);
                    }
                    EditorTool::Shape(_) | EditorTool::Redact | EditorTool::Crop => {
                        self.drag_start = Some(pt);
                        self.drag_current = Some(pt);
                    }
                }
                None
            }
            EditorMessage::PointerMove(pt) => {
                match self.active_tool {
                    EditorTool::Pen | EditorTool::Highlighter | EditorTool::Spray => {
                        if !self.in_progress_points.is_empty() {
                            self.in_progress_points.push(pt);
                        }
                    }
                    EditorTool::CurvedLine => {
                        if !self.in_progress_points.is_empty() {
                            self.in_progress_points.push(pt);
                            if !self.curve_drag_mode && self.curve_waypoints.is_empty() {
                                if let Some(start) = self.drag_start {
                                    let dx = pt.x - start.x;
                                    let dy = pt.y - start.y;
                                    if dx * dx + dy * dy >= 64.0 {
                                        self.curve_drag_mode = true;
                                    }
                                }
                            }
                        }
                        self.drag_current = Some(pt);
                    }
                    EditorTool::Eraser => {
                        let radius = (self.stroke_width * 2.0).max(10.0);
                        self.scene.erase_at(pt, radius);
                    }
                    EditorTool::Shape(_) | EditorTool::Redact | EditorTool::Crop => {
                        if self.drag_start.is_some() {
                            self.drag_current = Some(pt);
                        }
                    }
                }
                None
            }
            EditorMessage::PointerHover(pt) => {
                if self.active_tool == EditorTool::CurvedLine && !self.curve_waypoints.is_empty() {
                    self.hover_point = Some(pt);
                }
                None
            }
            EditorMessage::PointerUp(pt) => {
                match self.active_tool {
                    EditorTool::Pen => {
                        if !self.in_progress_points.is_empty() {
                            let points = std::mem::take(&mut self.in_progress_points);
                            self.scene.push_op(AnnotationOp::Pen {
                                points,
                                color: self.active_color,
                                width: self.stroke_width,
                                style: self.line_style,
                            });
                        }
                    }
                    EditorTool::Highlighter => {
                        if !self.in_progress_points.is_empty() {
                            let points = std::mem::take(&mut self.in_progress_points);
                            self.scene.push_op(AnnotationOp::Highlighter {
                                points,
                                color: self.active_color,
                                width: (self.stroke_width * 3.0).max(12.0),
                                opacity: 0.4,
                            });
                        }
                    }
                    EditorTool::Spray => {
                        if !self.in_progress_points.is_empty() {
                            let points = std::mem::take(&mut self.in_progress_points);
                            self.scene.push_op(AnnotationOp::Spray {
                                points,
                                color: self.active_color,
                                radius: (self.stroke_width * 3.0).max(8.0),
                            });
                        }
                    }
                    EditorTool::CurvedLine => {
                        let points = std::mem::take(&mut self.in_progress_points);
                        let was_drag = self.curve_drag_mode;
                        self.curve_drag_mode = false;
                        self.drag_start = None;
                        self.drag_current = None;
                        if was_drag && points.len() >= 2 {
                            self.scene.push_op(AnnotationOp::CurvedLine {
                                points,
                                color: self.active_color,
                                width: self.stroke_width,
                                style: self.line_style,
                            });
                        } else if let Some(last) = self.curve_waypoints.last().copied() {
                            let d_sq = (pt.x - last.x).powi(2) + (pt.y - last.y).powi(2);
                            if self.curve_waypoints.len() >= 2 && d_sq <= 64.0 {
                                self.commit_curve_waypoints();
                            } else if d_sq >= 4.0 {
                                self.curve_waypoints.push(pt);
                            }
                        } else {
                            self.curve_waypoints.push(pt);
                        }
                    }
                    EditorTool::Eraser => {}
                    EditorTool::Shape(kind) => {
                        if let Some(start) = self.drag_start.take() {
                            self.drag_current = None;
                            self.scene.push_op(AnnotationOp::Shape {
                                kind,
                                start,
                                end: pt,
                                color: self.active_color,
                                width: self.stroke_width,
                                style: self.line_style,
                            });
                        }
                    }
                    EditorTool::Redact => {
                        if let Some(start) = self.drag_start.take() {
                            self.drag_current = None;
                            let rect = Rect::new(start.x, start.y, pt.x - start.x, pt.y - start.y)
                                .normalized();
                            if rect.width >= 2.0 && rect.height >= 2.0 {
                                self.scene.push_op(AnnotationOp::RedactBox { rect });
                            }
                        }
                    }
                    EditorTool::Crop => {
                        if let Some(start) = self.drag_start.take() {
                            self.drag_current = None;
                            let rect = Rect::new(start.x, start.y, pt.x - start.x, pt.y - start.y)
                                .normalized()
                                .clamp(self.width as f32, self.height as f32);
                            if rect.width >= 4.0 && rect.height >= 4.0 {
                                self.scene.set_crop(Some(rect));
                                self.sync_bg_handle();
                            }
                        }
                    }
                }
                None
            }
            EditorMessage::FinishCurve => {
                self.commit_curve_waypoints();
                None
            }
            EditorMessage::CancelCurve => {
                self.curve_waypoints.clear();
                self.hover_point = None;
                self.curve_drag_mode = false;
                None
            }
            EditorMessage::Undo => {
                self.commit_curve_waypoints();
                let prev_crop = self.scene.crop_rect;
                self.scene.undo();
                if self.scene.crop_rect != prev_crop {
                    self.sync_bg_handle();
                }
                None
            }
            EditorMessage::Redo => {
                self.commit_curve_waypoints();
                let prev_crop = self.scene.crop_rect;
                self.scene.redo();
                if self.scene.crop_rect != prev_crop {
                    self.sync_bg_handle();
                }
                None
            }
            EditorMessage::ResetCrop => {
                if self.scene.crop_rect.is_some() {
                    self.scene.set_crop(None);
                    self.sync_bg_handle();
                }
                None
            }
            EditorMessage::CopyAndExit => {
                self.commit_curve_waypoints();
                match render_and_export(
                    &self.base_rgba,
                    self.width,
                    self.height,
                    &self.scene,
                    ExportFormat::Png,
                ) {
                    Ok(png_bytes) => Some(EditorAction::CopyAndExit(png_bytes)),
                    Err(err) => {
                        self.status_text = Some(format!("Export failed: {err}"));
                        None
                    }
                }
            }
            EditorMessage::SaveAs => {
                self.commit_curve_waypoints();
                Some(EditorAction::RunSaveAs {
                    base_rgba: self.base_rgba.clone(),
                    width: self.width,
                    height: self.height,
                    scene: self.scene.clone(),
                })
            }
            EditorMessage::SaveCompleted(res) => {
                match res {
                    Ok(Some(path)) => {
                        self.status_text = Some(format!("Saved to {}", path.display()));
                    }
                    Ok(None) => {
                        self.status_text = Some("Save cancelled".to_string());
                    }
                    Err(err) => {
                        self.status_text = Some(format!("Save failed: {err}"));
                    }
                }
                None
            }
            EditorMessage::NewCapture => {
                self.commit_curve_waypoints();
                Some(EditorAction::NewCapture)
            }
            EditorMessage::OpenHistory => {
                self.commit_curve_waypoints();
                Some(EditorAction::OpenHistory)
            }
            EditorMessage::ToggleTheme => {
                self.theme = match self.theme {
                    UiTheme::Light => UiTheme::Dark,
                    UiTheme::Dark => UiTheme::Light,
                };
                None
            }
        }
    }

    /// Constrói a árvore de interface completa do editor no layout Opção D (HeaderBar unificada no topo, Dock Vertical de Ferramentas integrado à esquerda, Canvas central rebaixado, Inspetor Vertical de Linha/Espessura/Cores integrado à direita e barra de status inferior).
    /// Input: `&self` | Output: `Element<'_, EditorMessage>`
    pub fn view(&self) -> Element<'_, EditorMessage> {
        let p = self.theme.palette();

        // Top Unified HeaderBar: Left (New | History | Undo/Redo | Reset Crop) — Center Title — Right (Theme | Save | Copy)
        let new_btn = button(
            row![
                icons::icon(icons::ICON_NEW, 14.0, p.accent),
                text("New").size(13).color(p.text),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(win11_card_btn(p))
        .on_press(EditorMessage::NewCapture);

        let history_btn = button(
            row![
                icons::icon(icons::ICON_HISTORY, 14.0, p.text),
                text("History").size(13).color(p.text),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(win11_card_btn(p))
        .on_press(EditorMessage::OpenHistory);

        let mut undo_btn = button(icons::icon(icons::ICON_UNDO, 14.0, p.text))
            .padding([6, 9])
            .style(win11_card_btn(p));
        if self.scene.can_undo() {
            undo_btn = undo_btn.on_press(EditorMessage::Undo);
        }

        let mut redo_btn = button(icons::icon(icons::ICON_REDO, 14.0, p.text))
            .padding([6, 9])
            .style(win11_card_btn(p));
        if self.scene.can_redo() {
            redo_btn = redo_btn.on_press(EditorMessage::Redo);
        }

        let mut left_header = row![new_btn, history_btn, v_divider(p), undo_btn, redo_btn]
            .spacing(6)
            .align_y(Alignment::Center);

        if self.scene.crop_rect.is_some() {
            left_header = left_header.push(
                button(text("Reset Crop").size(12))
                    .padding([6, 10])
                    .style(win11_card_btn(p))
                    .on_press(EditorMessage::ResetCrop),
            );
        }

        let theme_icon = match self.theme {
            UiTheme::Light => icons::ICON_THEME_MOON,
            UiTheme::Dark => icons::ICON_THEME_SUN,
        };
        let theme_btn = button(icons::icon(theme_icon, 14.0, p.text))
            .padding([6, 9])
            .style(win11_card_btn(p))
            .on_press(EditorMessage::ToggleTheme);

        let save_btn = button(
            row![
                icons::icon(icons::ICON_SAVE, 14.0, p.text),
                text("Save").size(13).color(p.text),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(win11_card_btn(p))
        .on_press(EditorMessage::SaveAs);

        let copy_btn = button(
            row![
                icons::icon(icons::ICON_COPY, 14.0, Color::WHITE),
                text("Copy").size(13),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 14])
        .style(win11_accent_btn(p))
        .on_press(EditorMessage::CopyAndExit);

        let right_header = row![theme_btn, save_btn, copy_btn]
            .spacing(6)
            .align_y(Alignment::Center);

        let header_bar = container(
            row![
                left_header,
                iced::widget::Space::with_width(Length::Fill),
                right_header,
            ]
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([7, 14])
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(p.header_bg)),
            border: iced::Border {
                color: p.card_border,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        });

        // 1. Integrated Left Tool Dock (Non-floating, flush with HeaderBar, left edge, and StatusBar)
        let left_dock_col = Column::new()
            .spacing(5)
            .align_x(Alignment::Center)
            .push(icon_tool_btn(
                p,
                icons::ICON_PEN,
                self.active_tool == EditorTool::Pen,
                EditorTool::Pen,
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_HIGHLIGHTER,
                self.active_tool == EditorTool::Highlighter,
                EditorTool::Highlighter,
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_SPRAY,
                self.active_tool == EditorTool::Spray,
                EditorTool::Spray,
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_ERASER,
                self.active_tool == EditorTool::Eraser,
                EditorTool::Eraser,
            ))
            .push(h_divider(p, 34))
            .push(icon_tool_btn(
                p,
                icons::ICON_RECT,
                self.active_tool == EditorTool::Shape(ShapeKind::Rectangle),
                EditorTool::Shape(ShapeKind::Rectangle),
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_ELLIPSE,
                self.active_tool == EditorTool::Shape(ShapeKind::Ellipse),
                EditorTool::Shape(ShapeKind::Ellipse),
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_LINE,
                self.active_tool == EditorTool::Shape(ShapeKind::Line),
                EditorTool::Shape(ShapeKind::Line),
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_CURVE,
                self.active_tool == EditorTool::CurvedLine,
                EditorTool::CurvedLine,
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_ARROW,
                self.active_tool == EditorTool::Shape(ShapeKind::Arrow),
                EditorTool::Shape(ShapeKind::Arrow),
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_DOUBLE_ARROW,
                self.active_tool == EditorTool::Shape(ShapeKind::DoubleArrow),
                EditorTool::Shape(ShapeKind::DoubleArrow),
            ))
            .push(h_divider(p, 34))
            .push(icon_tool_btn(
                p,
                icons::ICON_REDACT,
                self.active_tool == EditorTool::Redact,
                EditorTool::Redact,
            ))
            .push(icon_tool_btn(
                p,
                icons::ICON_CROP,
                self.active_tool == EditorTool::Crop,
                EditorTool::Crop,
            ));

        let left_dock = container(left_dock_col)
            .width(Length::Fixed(56.0))
            .height(Length::Fill)
            .padding([12, 8])
            .center_x(Length::Fixed(56.0))
            .style(move |_theme| container::Style {
                background: Some(iced::Background::Color(p.header_bg)),
                border: iced::Border {
                    color: p.card_border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

        // 2. Central Recessed Canvas Well
        let bg_image = iced::widget::container(
            iced::widget::image(self.bg_handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::ScaleDown),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(12)
        .center_x(Length::Fill)
        .center_y(Length::Fill);

        let canvas_el = iced::widget::canvas(EditorCanvas { state: self })
            .width(Length::Fill)
            .height(Length::Fill);

        let workspace = iced::widget::container(iced::widget::stack![bg_image, canvas_el])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_theme| container::Style {
                background: Some(iced::Background::Color(p.workspace_bg)),
                ..Default::default()
            });

        // 3. Integrated Right Inspector Dock (LINHA, ESPESSURA, CORES in 2-col grid)
        let mut color_items: Vec<Element<'_, EditorMessage>> = vec![
            color_swatch(p, SceneColor::BLACK, self.active_color == SceneColor::BLACK).into(),
            color_swatch(p, SceneColor::RED, self.active_color == SceneColor::RED).into(),
            color_swatch(
                p,
                SceneColor::YELLOW,
                self.active_color == SceneColor::YELLOW,
            )
            .into(),
            color_swatch(
                p,
                SceneColor::rgb(37, 99, 235),
                self.active_color == SceneColor::rgb(37, 99, 235),
            )
            .into(),
            color_swatch(
                p,
                SceneColor::rgb(34, 197, 94),
                self.active_color == SceneColor::rgb(34, 197, 94),
            )
            .into(),
            color_swatch(p, SceneColor::WHITE, self.active_color == SceneColor::WHITE).into(),
        ];

        for &custom in &self.custom_colors {
            color_items.push(color_swatch(p, custom, self.active_color == custom).into());
        }
        if self.custom_colors.len() < MAX_CUSTOM_COLORS {
            let next_slot = self.custom_colors.len();
            color_items.push(add_custom_color_btn(p, next_slot).into());
        }

        let mut colors_grid = Column::new().spacing(6).align_x(Alignment::Center);
        let mut iter = color_items.into_iter();
        while let Some(first) = iter.next() {
            let mut pair_row = row![first].spacing(6).align_y(Alignment::Center);
            if let Some(second) = iter.next() {
                pair_row = pair_row.push(second);
            }
            colors_grid = colors_grid.push(pair_row);
        }

        let right_dock_col = Column::new()
            .spacing(5)
            .align_x(Alignment::Center)
            .push(text("LINHA").size(10).color(p.text_muted))
            .push(line_style_btn(
                p,
                icons::ICON_LINE_SOLID,
                self.line_style == LineStyle::Solid,
                LineStyle::Solid,
            ))
            .push(line_style_btn(
                p,
                icons::ICON_LINE_DOTTED,
                self.line_style == LineStyle::Dotted,
                LineStyle::Dotted,
            ))
            .push(line_style_btn(
                p,
                icons::ICON_LINE_DASHED,
                self.line_style == LineStyle::Dashed,
                LineStyle::Dashed,
            ))
            .push(h_divider(p, 58))
            .push(text("ESPESSURA").size(9).color(p.text_muted))
            .push(width_btn(
                p,
                "S  2px",
                2.0,
                (self.stroke_width - 2.0).abs() < 0.1,
            ))
            .push(width_btn(
                p,
                "M  4px",
                4.0,
                (self.stroke_width - 4.0).abs() < 0.1,
            ))
            .push(width_btn(
                p,
                "L  8px",
                8.0,
                (self.stroke_width - 8.0).abs() < 0.1,
            ))
            .push(h_divider(p, 58))
            .push(text("CORES").size(10).color(p.text_muted))
            .push(colors_grid);

        let right_dock = container(right_dock_col)
            .width(Length::Fixed(84.0))
            .height(Length::Fill)
            .padding([12, 8])
            .center_x(Length::Fixed(84.0))
            .style(move |_theme| container::Style {
                background: Some(iced::Background::Color(p.header_bg)),
                border: iced::Border {
                    color: p.card_border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

        let body_row = row![left_dock, workspace, right_dock]
            .width(Length::Fill)
            .height(Length::Fill);

        let live_crop_dims = if self.active_tool == EditorTool::Crop {
            match (self.drag_start, self.drag_current) {
                (Some(start), Some(end)) => {
                    let rect = Rect::new(start.x, start.y, end.x - start.x, end.y - start.y);
                    let (_, _, pw, ph, valid) = self.resolve_viewport_bounds(Some(rect));
                    valid.then_some((pw, ph))
                }
                _ => None,
            }
        } else {
            None
        };
        let dim_label = if let Some((pw, ph)) = live_crop_dims {
            format!("{pw} × {ph} px (selecionando...)")
        } else {
            let (_, _, vw, vh, is_cropped) = self.visible_crop_bounds();
            if is_cropped {
                format!("{vw} × {vh} px (recortado)")
            } else {
                format!("{} × {} px", self.width, self.height)
            }
        };
        let status_msg = self.format_status_banner();
        let status_bar = container(
            row![
                text(status_msg).size(12).color(p.text_muted),
                iced::widget::Space::with_width(Length::Fill),
                text(dim_label).size(12).color(p.text_muted),
            ]
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([5, 14])
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(p.header_bg)),
            border: iced::Border {
                color: p.card_border,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        });

        let col = Column::new()
            .push(header_bar)
            .push(body_row)
            .push(status_bar);

        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Formata a mensagem da barra de status combinando o estado atual e a ferramenta/linha/espessura selecionada.
    /// Input: `&self` | Output: `String` descritiva para o rodapé.
    fn format_status_banner(&self) -> String {
        let base = self.status_text.as_deref().unwrap_or("Ready");
        let tool_name = match self.active_tool {
            EditorTool::Pen => "Caneta",
            EditorTool::Highlighter => "Marca-texto",
            EditorTool::Spray => "Spray",
            EditorTool::Eraser => "Borracha",
            EditorTool::CurvedLine => "Linha Curva",
            EditorTool::Shape(ShapeKind::Rectangle) => "Retângulo",
            EditorTool::Shape(ShapeKind::Ellipse) => "Esfera",
            EditorTool::Shape(ShapeKind::Line) => "Linha",
            EditorTool::Shape(ShapeKind::Arrow) => "Seta",
            EditorTool::Shape(ShapeKind::DoubleArrow) => "Seta Dupla",
            EditorTool::Redact => "Vidro Fosco",
            EditorTool::Crop => "Crop",
        };
        let style_name = match self.line_style {
            LineStyle::Solid => "Sólida",
            LineStyle::Dotted => "Pontilhada",
            LineStyle::Dashed => "Tracejada",
        };
        let w_int = self.stroke_width.round() as u32;
        let w_tag = if w_int <= 2 {
            "S"
        } else if w_int <= 4 {
            "M"
        } else {
            "L"
        };
        if self.active_tool == EditorTool::CurvedLine {
            format!("{base}  •  {tool_name} ({style_name} • {w_tag} {w_int}px) — Clique nos pontos (duplo clique, botão direito ou Enter finaliza) ou arraste")
        } else {
            format!("{base}  •  Ferramenta: {tool_name} ({style_name} • {w_tag} {w_int}px)")
        }
    }
}

/// Abre o diálogo nativo `Salvar Como` do portal XDG e grava a imagem rasterizada em PNG ou JPEG no caminho escolhido.
/// Input: `base_rgba` (`Vec<u8>`), `width` (`u32`), `height` (`u32`), `scene` (`AnnotationScene`) | Output: `Result<Option<PathBuf>, String>` com caminho salvo ou `None` se cancelado
#[cfg(target_os = "linux")]
pub async fn save_as_via_portal(
    base_rgba: Vec<u8>,
    width: u32,
    height: u32,
    scene: AnnotationScene,
) -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let request = SelectedFiles::save_file()
        .title("Save Snip")
        .current_name("snip.png")
        .filter(FileFilter::new("PNG Image").glob("*.png"))
        .filter(FileFilter::new("JPEG Image").glob("*.jpg").glob("*.jpeg"))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let files = match request.response() {
        Ok(f) => f,
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };

    let Some(uri) = files.uris().first() else {
        return Ok(None);
    };
    let path = uri
        .to_file_path()
        .map_err(|()| format!("portal returned non-file URI: {uri}"))?;

    let format = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => ExportFormat::Jpeg {
            quality: DEFAULT_JPEG_QUALITY,
        },
        _ => ExportFormat::Png,
    };

    let encoded =
        render_and_export(&base_rgba, width, height, &scene, format).map_err(|e| e.to_string())?;
    std::fs::write(&path, encoded).map_err(|e| e.to_string())?;
    Ok(Some(path))
}

/// Abre o diálogo nativo `GetSaveFileNameW` no Windows 11 e grava a imagem rasterizada em PNG ou JPEG.
/// Input: `base_rgba` (`Vec<u8>`), `width` (`u32`), `height` (`u32`), `scene` (`AnnotationScene`) | Output: `Result<Option<PathBuf>, String>`.
#[cfg(target_os = "windows")]
pub async fn save_as_via_portal(
    base_rgba: Vec<u8>,
    width: u32,
    height: u32,
    scene: AnnotationScene,
) -> Result<Option<PathBuf>, String> {
    tokio::task::spawn_blocking(move || -> Result<Option<PathBuf>, String> {
        use windows::core::{HSTRING, PCWSTR, PWSTR};
        use windows::Win32::UI::Controls::Dialogs::{
            GetSaveFileNameW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
            OPENFILENAMEW,
        };

        let filter: Vec<u16> =
            "PNG Image (*.png)\0*.png\0JPEG Image (*.jpg;*.jpeg)\0*.jpg;*.jpeg\0\0"
                .encode_utf16()
                .collect();
        let title = HSTRING::from("Save Snip");
        let def_ext = HSTRING::from("png");
        let mut file_buf = vec![0u16; 1024];
        for (i, ch) in "snip.png".encode_utf16().enumerate() {
            file_buf[i] = ch;
        }

        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            nFilterIndex: 1,
            lpstrFile: PWSTR(file_buf.as_mut_ptr()),
            nMaxFile: file_buf.len() as u32,
            lpstrTitle: PCWSTR(title.as_ptr()),
            Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
            lpstrDefExt: PCWSTR(def_ext.as_ptr()),
            ..Default::default()
        };

        if !unsafe { GetSaveFileNameW(&mut ofn) }.as_bool() {
            return Ok(None);
        }

        let nul_pos = file_buf
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(file_buf.len());
        let mut path = PathBuf::from(String::from_utf16_lossy(&file_buf[..nul_pos]));
        if path.extension().is_none() {
            path.set_extension(if ofn.nFilterIndex == 2 { "jpg" } else { "png" });
        }

        let format = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg" | "jpeg") => ExportFormat::Jpeg {
                quality: DEFAULT_JPEG_QUALITY,
            },
            _ => ExportFormat::Png,
        };

        let encoded = render_and_export(&base_rgba, width, height, &scene, format)
            .map_err(|e| e.to_string())?;
        std::fs::write(&path, encoded).map_err(|e| e.to_string())?;
        Ok(Some(path))
    })
    .await
    .map_err(|e| format!("Windows save dialog task panicked: {e}"))?
}

/// Fallback neutro de diálogo `Salvar Como` em sistemas operacionais sem backend de janela nativo implementado.
/// Input: `_base_rgba`, `_width`, `_height`, `_scene` | Output: `Err` indicando ausência de suporte.
#[cfg(all(not(target_os = "linux"), not(target_os = "windows")))]
pub async fn save_as_via_portal(
    _base_rgba: Vec<u8>,
    _width: u32,
    _height: u32,
    _scene: AnnotationScene,
) -> Result<Option<PathBuf>, String> {
    Err("Save dialog unsupported on this platform".to_string())
}

/// Constrói um divisor vertical fino de 1 px estilizado conforme a paleta do tema.
/// Input: `p` (`ThemePalette`) | Output: `Element<'static, EditorMessage>`
fn v_divider(p: ThemePalette) -> Element<'static, EditorMessage> {
    container(iced::widget::Space::with_width(1).height(20))
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(p.card_border)),
            ..Default::default()
        })
        .into()
}

/// Constrói um divisor horizontal fino de 1 px para separar seções nas docks laterais integradas.
/// Input: `p` (`ThemePalette`), `width` (`u16`) | Output: `Element<'static, EditorMessage>`
fn h_divider(p: ThemePalette, width: u16) -> Element<'static, EditorMessage> {
    container(iced::widget::Space::with_width(width).height(1))
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(p.card_border)),
            ..Default::default()
        })
        .into()
}

/// Retorna o estilo Fluent de destaque com cor de acento para ações primárias do editor.
/// Input: `p` (`ThemePalette`) | Output: função de estilização para `iced::widget::button`
fn win11_accent_btn(
    p: ThemePalette,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style + Copy {
    move |_theme, status| {
        let base = match status {
            button::Status::Hovered => p.accent_hover,
            _ => p.accent,
        };
        button::Style {
            background: Some(iced::Background::Color(base)),
            text_color: Color::WHITE,
            border: iced::Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Retorna o estilo Fluent de cartão com borda sutil para botões secundários da barra.
/// Input: `p` (`ThemePalette`) | Output: função de estilização para `iced::widget::button`
fn win11_card_btn(
    p: ThemePalette,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style + Copy {
    move |_theme, status| {
        let bg = match status {
            button::Status::Hovered => p.card_hover,
            button::Status::Pressed => p.pill_bg,
            _ => p.card_bg,
        };
        button::Style {
            background: Some(iced::Background::Color(bg)),
            text_color: p.text,
            border: iced::Border {
                radius: 6.0.into(),
                color: p.card_border,
                width: 1.0,
            },
            ..Default::default()
        }
    }
}

/// Retorna o estilo plano Fluent com estado ativo opcional para botões de ferramenta e utilitários.
/// Input: `p` (`ThemePalette`), `active` (`bool`) | Output: função de estilização para `iced::widget::button`
fn win11_flat_btn(
    p: ThemePalette,
    active: bool,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style + Copy {
    move |_theme, status| {
        let bg = if active {
            p.active_bg
        } else {
            match status {
                button::Status::Hovered => p.pill_bg,
                button::Status::Pressed => p.card_border,
                _ => Color::TRANSPARENT,
            }
        };
        let fg = if matches!(status, button::Status::Disabled) {
            p.text_muted
        } else {
            p.text
        };
        button::Style {
            background: Some(iced::Background::Color(bg)),
            text_color: fg,
            border: iced::Border {
                radius: 6.0.into(),
                color: if active { p.accent } else { Color::TRANSPARENT },
                width: if active { 1.0 } else { 0.0 },
            },
            ..Default::default()
        }
    }
}

/// Abre um seletor padrão de cores do sistema (`GTK4 ColorDialog`/`zenity` no Linux; `ChooseColorW` no Windows) e retorna a cor escolhida.
/// Input: `initial` (`SceneColor`) | Output: `Option<SceneColor>` com a cor escolhida ou `None` se cancelado.
pub async fn pick_color_via_system(initial: SceneColor) -> Option<SceneColor> {
    tokio::task::spawn_blocking(move || pick_color_blocking(initial))
        .await
        .ok()
        .flatten()
}

/// Abre o diálogo nativo `ChooseColorW` do Win32 pré-carregado com `initial`.
/// Input: `initial` (`SceneColor`) | Output: `Option<SceneColor>` escolhida pelo usuário.
#[cfg(target_os = "windows")]
fn pick_color_blocking(initial: SceneColor) -> Option<SceneColor> {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::Controls::Dialogs::{
        ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW,
    };

    let mut custom = [COLORREF(0x00FF_FFFF); 16];
    let init_ref =
        COLORREF(u32::from(initial.r) | (u32::from(initial.g) << 8) | (u32::from(initial.b) << 16));
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: HWND::default(),
        rgbResult: init_ref,
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_FULLOPEN | CC_RGBINIT,
        ..Default::default()
    };

    if unsafe { ChooseColorW(&mut cc) }.as_bool() {
        let raw = cc.rgbResult.0;
        Some(SceneColor::rgb(
            (raw & 0xFF) as u8,
            ((raw >> 8) & 0xFF) as u8,
            ((raw >> 16) & 0xFF) as u8,
        ))
    } else {
        None
    }
}

/// Fallback neutro de seletor de cores quando não está nem no Linux nem no Windows.
/// Input: `_initial` (`SceneColor`) | Output: `None`.
#[cfg(all(not(target_os = "linux"), not(target_os = "windows")))]
fn pick_color_blocking(_initial: SceneColor) -> Option<SceneColor> {
    None
}

/// Executa o seletor de cores nativo do sistema operacional no Linux de forma síncrona em thread separada.
/// Input: `initial` (`SceneColor`) | Output: `Option<SceneColor>` escolhida pelo usuário.
#[cfg(target_os = "linux")]
fn pick_color_blocking(initial: SceneColor) -> Option<SceneColor> {
    let hex_arg = format!("#{:02x}{:02x}{:02x}", initial.r, initial.g, initial.b);
    let gtk_script = r##"
import sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, Gdk, GLib

initial_hex = sys.argv[1] if len(sys.argv) > 1 else "#dc2626"
loop = GLib.MainLoop()
chosen = [None]

def on_activate(app):
    win = Gtk.Window(application=app, default_width=1, default_height=1, decorated=False)
    rgba = Gdk.RGBA()
    rgba.parse(initial_hex)
    if hasattr(Gtk, "ColorDialog"):
        dlg = Gtk.ColorDialog(with_alpha=False)
        def on_finish(d, res):
            try:
                c = d.choose_rgba_finish(res)
                chosen[0] = (int(round(c.red * 255)), int(round(c.green * 255)), int(round(c.blue * 255)))
            except Exception:
                pass
            loop.quit()
        dlg.choose_rgba(win, rgba, None, on_finish)
    else:
        dlg = Gtk.ColorChooserDialog(title="Escolher cor", transient_for=win, modal=True)
        dlg.set_use_alpha(False)
        dlg.set_rgba(rgba)
        def on_resp(d, resp):
            if resp == Gtk.ResponseType.OK:
                c = d.get_rgba()
                chosen[0] = (int(round(c.red * 255)), int(round(c.green * 255)), int(round(c.blue * 255)))
            d.destroy()
            loop.quit()
        dlg.connect("response", on_resp)
        dlg.present()

app = Gtk.Application()
app.connect("activate", on_activate)
GLib.idle_add(lambda: app.register(None) or app.activate() or False)
loop.run()
if chosen[0] is not None:
    r, g, b = chosen[0]
    print(f"{r},{g},{b}", flush=True)
"##;

    if let Ok(out) = std::process::Command::new("python3")
        .args(["-u", "-c", gtk_script, &hex_arg])
        .output()
    {
        if out.status.success() {
            if let Some(c) = parse_rgb_triplet(&String::from_utf8_lossy(&out.stdout)) {
                return Some(c);
            }
        }
    }

    if let Ok(out) = std::process::Command::new("zenity")
        .args(["--color-selection", &format!("--color={hex_arg}")])
        .output()
    {
        if out.status.success() {
            if let Some(c) = parse_zenity_color(&String::from_utf8_lossy(&out.stdout)) {
                return Some(c);
            }
        }
    }

    None
}

/// Converte uma string `"R,G,B"` emitida pelo seletor GTK4 em `SceneColor`.
/// Input: `raw` (`&str`) | Output: `Option<SceneColor>`.
#[cfg(target_os = "linux")]
fn parse_rgb_triplet(raw: &str) -> Option<SceneColor> {
    let line = raw.lines().next()?.trim();
    let mut parts = line.split(',');
    let r: u8 = parts.next()?.trim().parse().ok()?;
    let g: u8 = parts.next()?.trim().parse().ok()?;
    let b: u8 = parts.next()?.trim().parse().ok()?;
    Some(SceneColor::rgb(r, g, b))
}

/// Converte uma saída `rgb(r,g,b)` ou `#rrggbb` do `zenity` em `SceneColor`.
/// Input: `raw` (`&str`) | Output: `Option<SceneColor>`.
#[cfg(target_os = "linux")]
fn parse_zenity_color(raw: &str) -> Option<SceneColor> {
    let s = raw.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() >= 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(SceneColor::rgb(r, g, b));
        }
    }
    if let Some(inner) = s
        .strip_prefix("rgb(")
        .or_else(|| s.strip_prefix("rgba("))
        .and_then(|v| v.strip_suffix(')'))
    {
        return parse_rgb_triplet(inner);
    }
    None
}

/// Cria um botão de ferramenta com ícone SVG que seleciona `tool` ao ser pressionado.
/// Input: `p` (`ThemePalette`), `svg_bytes` (`&'static [u8]`), `active` (`bool`), `tool` (`EditorTool`) | Output: `button::Button<'static, EditorMessage>`
fn icon_tool_btn(
    p: ThemePalette,
    svg_bytes: &'static [u8],
    active: bool,
    tool: EditorTool,
) -> button::Button<'static, EditorMessage> {
    let fg = if active { p.accent } else { p.text };
    button(icons::icon(svg_bytes, 16.0, fg))
        .padding([6, 9])
        .style(win11_flat_btn(p, active))
        .on_press(EditorMessage::SelectTool(tool))
}

/// Cria um botão com ícone SVG que seleciona o estilo de linha (`LineStyle`) para traços e formas.
/// Input: `p` (`ThemePalette`), `svg_bytes` (`&'static [u8]`), `active` (`bool`), `style` (`LineStyle`) | Output: `button::Button<'static, EditorMessage>`
fn line_style_btn(
    p: ThemePalette,
    svg_bytes: &'static [u8],
    active: bool,
    style: LineStyle,
) -> button::Button<'static, EditorMessage> {
    let fg = if active { p.accent } else { p.text };
    button(
        container(icons::icon(svg_bytes, 16.0, fg))
            .width(Length::Fill)
            .center_x(Length::Fill),
    )
    .width(Length::Fixed(58.0))
    .padding([5, 8])
    .style(win11_flat_btn(p, active))
    .on_press(EditorMessage::SelectLineStyle(style))
}

/// Cria o botão `+` para adicionar uma nova cor personalizada abrindo o seletor de cores do sistema.
/// Input: `p` (`ThemePalette`), `slot` (`usize`) | Output: `button::Button<'static, EditorMessage>`
fn add_custom_color_btn(p: ThemePalette, slot: usize) -> button::Button<'static, EditorMessage> {
    button(icons::icon(icons::ICON_PALETTE_ADD, 16.0, p.text))
        .padding(3)
        .style(win11_flat_btn(p, false))
        .on_press(EditorMessage::RequestCustomColor(slot))
}

/// Cria um seletor circular colorido para alterar a cor do traço ativo no editor.
/// Input: `p` (`ThemePalette`), `color` (`SceneColor`), `active` (`bool`) | Output: `button::Button<'static, EditorMessage>`
fn color_swatch(
    p: ThemePalette,
    color: SceneColor,
    active: bool,
) -> button::Button<'static, EditorMessage> {
    let swatch_color = Color::from_rgb8(color.r, color.g, color.b);
    let inner = container(iced::widget::Space::with_width(14).height(14)).style(move |_theme| {
        container::Style {
            background: Some(iced::Background::Color(swatch_color)),
            border: iced::Border {
                radius: 999.0.into(),
                color: p.card_border,
                width: 1.0,
            },
            ..Default::default()
        }
    });
    button(inner)
        .padding(3)
        .style(move |_theme, status| {
            let bg = if active {
                p.active_bg
            } else if matches!(status, button::Status::Hovered) {
                p.pill_bg
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(iced::Background::Color(bg)),
                border: iced::Border {
                    radius: 999.0.into(),
                    color: if active { p.accent } else { Color::TRANSPARENT },
                    width: if active { 1.5 } else { 0.0 },
                },
                ..Default::default()
            }
        })
        .on_press(EditorMessage::SelectColor(color))
}

/// Cria um botão compacto de texto que aplica uma espessura de traço predefinida em pixels.
/// Input: `p` (`ThemePalette`), `label` (`&str`), `width` (`f32`), `active` (`bool`) | Output: `button::Button<'_, EditorMessage>`
fn width_btn(
    p: ThemePalette,
    label: &str,
    width: f32,
    active: bool,
) -> button::Button<'_, EditorMessage> {
    button(
        container(text(label).size(11))
            .width(Length::Fill)
            .center_x(Length::Fill),
    )
    .width(Length::Fixed(58.0))
    .padding([4, 6])
    .style(win11_flat_btn(p, active))
    .on_press(EditorMessage::SetStrokeWidth(width))
}

/// Programa `iced::widget::canvas` responsável por renderizar as anotações sobre a imagem ajustada e capturar o ponteiro.
/// Armazena: `state` (`&'a EditorState`) | Principais métodos: `fit_rect`, `to_image_point`, `update`, `draw`
struct EditorCanvas<'a> {
    state: &'a EditorState,
}

impl<'a> EditorCanvas<'a> {
    /// Calcula o retângulo centralizado onde a área visível (recortada ou integral) cabe mantendo proporção dentro dos limites disponíveis.
    /// Input: `&self`, `bounds` (`Rectangle`) | Output: `Rectangle` ajustado no canvas
    fn fit_rect(&self, bounds: Rectangle) -> Rectangle {
        let (_, _, vw, vh, _) = self.state.visible_crop_bounds();
        let view_w = vw.max(1) as f32;
        let view_h = vh.max(1) as f32;
        let pad = 24.0;
        let avail_w = (bounds.width - pad).max(1.0);
        let avail_h = (bounds.height - pad).max(1.0);
        let scale = (avail_w / view_w).min(avail_h / view_h).min(1.0);
        let draw_w = view_w * scale;
        let draw_h = view_h * scale;
        Rectangle {
            x: (bounds.width - draw_w) * 0.5,
            y: (bounds.height - draw_h) * 0.5,
            width: draw_w,
            height: draw_h,
        }
    }

    /// Converte uma posição do ponteiro no canvas para coordenadas limitadas à área visível no espaço da imagem base.
    /// Input: `&self`, `fit` (`Rectangle`), `pos` (`IcedPoint`) | Output: `Point` no espaço da imagem
    fn to_image_point(&self, fit: Rectangle, pos: IcedPoint) -> Point {
        let (vx, vy, vw, vh, _) = self.state.visible_crop_bounds();
        let vx_f = vx as f32;
        let vy_f = vy as f32;
        let vw_f = vw.max(1) as f32;
        let vh_f = vh.max(1) as f32;
        let x = vx_f + ((pos.x - fit.x) / fit.width.max(1.0)) * vw_f;
        let y = vy_f + ((pos.y - fit.y) / fit.height.max(1.0)) * vh_f;
        Point::new(
            x.clamp(vx_f, (vx + vw) as f32),
            y.clamp(vy_f, (vy + vh) as f32),
        )
    }
}

impl<'a> canvas::Program<EditorMessage> for EditorCanvas<'a> {
    type State = bool;

    /// Captura eventos de clique, arraste e soltura do mouse no canvas convertendo-os em `EditorMessage`.
    /// Input: `&self`, `is_dragging` (`&mut bool`), `event` (`canvas::Event`), `bounds` (`Rectangle`), `cursor` (`mouse::Cursor`) | Output: `(event::Status, Option<EditorMessage>)`
    fn update(
        &self,
        is_dragging: &mut Self::State,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (event::Status, Option<EditorMessage>) {
        let fit = self.fit_rect(bounds);
        let pos = match cursor.position_in(bounds) {
            Some(p) => p,
            None if *is_dragging => match cursor.position() {
                Some(abs) => IcedPoint::new(
                    (abs.x - bounds.x).clamp(0.0, bounds.width),
                    (abs.y - bounds.y).clamp(0.0, bounds.height),
                ),
                None => return (event::Status::Ignored, None),
            },
            None => return (event::Status::Ignored, None),
        };
        let img_pt = self.to_image_point(fit, pos);

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                *is_dragging = true;
                (
                    event::Status::Captured,
                    Some(EditorMessage::PointerDown(img_pt)),
                )
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
                if self.state.active_tool == EditorTool::CurvedLine
                    && !self.state.curve_waypoints.is_empty() =>
            {
                (event::Status::Captured, Some(EditorMessage::FinishCurve))
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) if *is_dragging => (
                event::Status::Captured,
                Some(EditorMessage::PointerMove(img_pt)),
            ),
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. })
                if self.state.active_tool == EditorTool::CurvedLine
                    && !self.state.curve_waypoints.is_empty() =>
            {
                (
                    event::Status::Captured,
                    Some(EditorMessage::PointerHover(img_pt)),
                )
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if *is_dragging =>
            {
                *is_dragging = false;
                (
                    event::Status::Captured,
                    Some(EditorMessage::PointerUp(img_pt)),
                )
            }
            _ => (event::Status::Ignored, None),
        }
    }

    /// Renderiza no `Frame` as operações confirmadas da cena, o traço/forma em andamento e a área visível recortada.
    /// Input: `&self`, `_state` (`&bool`), `renderer` (`&iced::Renderer`), `_theme` (`&iced::Theme`), `bounds` (`Rectangle`), `_cursor` (`mouse::Cursor`) | Output: `Vec<Geometry>`
    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let fit = self.fit_rect(bounds);
        let (vx, vy, vw, _vh, _) = self.state.visible_crop_bounds();
        let scale = fit.width / (vw.max(1) as f32);
        // `Frame::with_clip` já translada o sub-frame para `(fit.x, fit.y)`.
        let local_origin_fit = Rectangle {
            x: -(vx as f32) * scale,
            y: -(vy as f32) * scale,
            width: fit.width,
            height: fit.height,
        };

        let base_info = (
            self.state.base_rgba.as_slice(),
            self.state.width,
            self.state.height,
        );

        frame.with_clip(fit, |clipped| {
            for op in self.state.scene.ops() {
                draw_scene_op(clipped, local_origin_fit, scale, op, Some(base_info));
            }

            // In-progress stroke/shape/redact/crop preview.
            match self.state.active_tool {
                EditorTool::Pen if self.state.in_progress_points.len() >= 2 => {
                    let op = AnnotationOp::Pen {
                        points: self.state.in_progress_points.clone(),
                        color: self.state.active_color,
                        width: self.state.stroke_width,
                        style: self.state.line_style,
                    };
                    draw_scene_op(clipped, local_origin_fit, scale, &op, None);
                }
                EditorTool::Highlighter if self.state.in_progress_points.len() >= 2 => {
                    let op = AnnotationOp::Highlighter {
                        points: self.state.in_progress_points.clone(),
                        color: self.state.active_color,
                        width: (self.state.stroke_width * 3.0).max(12.0),
                        opacity: 0.4,
                    };
                    draw_scene_op(clipped, local_origin_fit, scale, &op, None);
                }
                EditorTool::Spray if !self.state.in_progress_points.is_empty() => {
                    let op = AnnotationOp::Spray {
                        points: self.state.in_progress_points.clone(),
                        color: self.state.active_color,
                        radius: (self.state.stroke_width * 3.0).max(8.0),
                    };
                    draw_scene_op(clipped, local_origin_fit, scale, &op, None);
                }
                EditorTool::CurvedLine
                    if self.state.curve_drag_mode && self.state.in_progress_points.len() >= 2 =>
                {
                    let op = AnnotationOp::CurvedLine {
                        points: self.state.in_progress_points.clone(),
                        color: self.state.active_color,
                        width: self.state.stroke_width,
                        style: self.state.line_style,
                    };
                    draw_scene_op(clipped, local_origin_fit, scale, &op, None);
                }
                EditorTool::Shape(kind) => {
                    if let (Some(start), Some(end)) =
                        (self.state.drag_start, self.state.drag_current)
                    {
                        let op = AnnotationOp::Shape {
                            kind,
                            start,
                            end,
                            color: self.state.active_color,
                            width: self.state.stroke_width,
                            style: self.state.line_style,
                        };
                        draw_scene_op(clipped, local_origin_fit, scale, &op, None);
                    }
                }
                EditorTool::Redact => {
                    if let (Some(start), Some(end)) =
                        (self.state.drag_start, self.state.drag_current)
                    {
                        let r = Rect::new(start.x, start.y, end.x - start.x, end.y - start.y)
                            .normalized();
                        let tl = to_canvas_pt(local_origin_fit, scale, Point::new(r.x, r.y));
                        let sz = Size::new(r.width * scale, r.height * scale);
                        clipped.fill_rectangle(
                            tl,
                            sz,
                            Color::from_rgba(12.0 / 255.0, 14.0 / 255.0, 18.0 / 255.0, 0.42),
                        );
                        let border_path = Path::rectangle(tl, sz);
                        let dot_segments = [0.5_f32, 6.0];
                        let dot_dash = LineDash {
                            segments: &dot_segments,
                            offset: 0,
                        };
                        clipped.stroke(
                            &border_path,
                            Stroke {
                                line_dash: dot_dash,
                                ..Stroke::default()
                                    .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.80))
                                    .with_width(3.0)
                                    .with_line_cap(LineCap::Round)
                            },
                        );
                        clipped.stroke(
                            &border_path,
                            Stroke {
                                line_dash: dot_dash,
                                ..Stroke::default()
                                    .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.96))
                                    .with_width(1.8)
                                    .with_line_cap(LineCap::Round)
                            },
                        );
                    }
                }
                EditorTool::Crop => {
                    if let (Some(start), Some(end)) =
                        (self.state.drag_start, self.state.drag_current)
                    {
                        let r = Rect::new(start.x, start.y, end.x - start.x, end.y - start.y)
                            .normalized();
                        let tl = to_canvas_pt(local_origin_fit, scale, Point::new(r.x, r.y));
                        let sz = Size::new(r.width * scale, r.height * scale);
                        let dim_col = Color::from_rgba(0.0, 0.0, 0.0, 0.45);
                        if tl.y > 0.0 {
                            clipped.fill_rectangle(
                                IcedPoint::new(0.0, 0.0),
                                Size::new(fit.width, tl.y),
                                dim_col,
                            );
                        }
                        let bottom_h = fit.height - (tl.y + sz.height);
                        if bottom_h > 0.0 {
                            clipped.fill_rectangle(
                                IcedPoint::new(0.0, tl.y + sz.height),
                                Size::new(fit.width, bottom_h),
                                dim_col,
                            );
                        }
                        if tl.x > 0.0 && sz.height > 0.0 {
                            clipped.fill_rectangle(
                                IcedPoint::new(0.0, tl.y),
                                Size::new(tl.x, sz.height),
                                dim_col,
                            );
                        }
                        let right_w = fit.width - (tl.x + sz.width);
                        if right_w > 0.0 && sz.height > 0.0 {
                            clipped.fill_rectangle(
                                IcedPoint::new(tl.x + sz.width, tl.y),
                                Size::new(right_w, sz.height),
                                dim_col,
                            );
                        }
                        let outer_path = Path::rectangle(tl, sz);
                        clipped.stroke(
                            &outer_path,
                            Stroke::default()
                                .with_color(Color::from_rgba(0.0, 0.47, 0.83, 0.95))
                                .with_width(2.0),
                        );
                        if sz.width > 2.0 && sz.height > 2.0 {
                            let inner_path = Path::rectangle(
                                IcedPoint::new(tl.x + 1.0, tl.y + 1.0),
                                Size::new(sz.width - 2.0, sz.height - 2.0),
                            );
                            clipped.stroke(
                                &inner_path,
                                Stroke::default()
                                    .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.9))
                                    .with_width(1.0),
                            );
                        }
                    }
                }
                _ => {}
            }

            if !self.state.curve_waypoints.is_empty() {
                if self.state.curve_waypoints.len() >= 2 {
                    let spline = catmull_rom_spline_points(&self.state.curve_waypoints);
                    draw_polyline(
                        clipped,
                        local_origin_fit,
                        scale,
                        &spline,
                        to_iced_color(self.state.active_color, 1.0),
                        self.state.stroke_width,
                        self.state.line_style,
                    );
                }
                if let Some(hover) = self.state.hover_point {
                    let mut preview = self.state.curve_waypoints.clone();
                    preview.push(hover);
                    let spline = catmull_rom_spline_points(&preview);
                    draw_polyline(
                        clipped,
                        local_origin_fit,
                        scale,
                        &spline,
                        to_iced_color(self.state.active_color, 0.55),
                        self.state.stroke_width,
                        self.state.line_style,
                    );
                }
                for &wp in &self.state.curve_waypoints {
                    let c = to_canvas_pt(local_origin_fit, scale, wp);
                    clipped.fill(&Path::circle(c, 4.5), Color::from_rgba(1.0, 1.0, 1.0, 0.95));
                    clipped.fill(
                        &Path::circle(c, 3.0),
                        to_iced_color(self.state.active_color, 1.0),
                    );
                }
            }
        });

        let border_path = Path::rectangle(
            IcedPoint::new(fit.x, fit.y),
            Size::new(fit.width, fit.height),
        );
        frame.stroke(
            &border_path,
            Stroke::default()
                .with_color(Color::from_rgba(0.5, 0.5, 0.55, 0.35))
                .with_width(1.0),
        );

        vec![frame.into_geometry()]
    }
}

/// Converte uma `SceneColor` com multiplicador de opacidade em `iced::Color`.
/// Input: `c` (`SceneColor`), `alpha_mul` (`f32`) | Output: `Color` normalizado entre `0.0` e `1.0`
fn to_iced_color(c: SceneColor, alpha_mul: f32) -> Color {
    Color::from_rgba8(
        c.r,
        c.g,
        c.b,
        ((f32::from(c.a) / 255.0) * alpha_mul).clamp(0.0, 1.0),
    )
}

/// Transforma um ponto de coordenadas de imagem em coordenadas do canvas usando o deslocamento `fit` e fator `scale`.
/// Input: `fit` (`Rectangle`), `scale` (`f32`), `pt` (`Point`) | Output: `IcedPoint` escalado
fn to_canvas_pt(fit: Rectangle, scale: f32, pt: Point) -> IcedPoint {
    IcedPoint::new(fit.x + pt.x * scale, fit.y + pt.y * scale)
}

/// Gera uma grade grosseira desfocada e escurecida a 50% sobre `base_rgba` para prévia rápida de vidro fosco no canvas Iced.
/// Input: `rgba`, dimensões `img_w`/`img_h`, limites inteiros `(ix0, iy0, ix1, iy1)` e tamanho de célula `cell` | Output: `(grade RGB [0..1], gw, gh)`.
#[allow(clippy::too_many_arguments)]
fn build_preview_smoked_grid(
    rgba: &[u8],
    img_w: u32,
    img_h: u32,
    ix0: i32,
    iy0: i32,
    ix1: i32,
    iy1: i32,
    cell: i32,
) -> (Vec<[f32; 3]>, usize, usize) {
    let bw = (ix1 - ix0).max(1);
    let bh = (iy1 - iy0).max(1);
    let gw = ((bw + cell - 1) / cell).max(1) as usize;
    let gh = ((bh + cell - 1) / cell).max(1) as usize;
    let mut raw_grid = vec![[30.0_f32 / 255.0, 32.0 / 255.0, 36.0 / 255.0]; gw * gh];

    for gy in 0..gh {
        let ys = (iy0 + (gy as i32) * cell).clamp(0, (img_h as i32) - 1);
        let ye = (iy0 + ((gy + 1) as i32) * cell).clamp(ys + 1, img_h as i32);
        for gx in 0..gw {
            let xs = (ix0 + (gx as i32) * cell).clamp(0, (img_w as i32) - 1);
            let xe = (ix0 + ((gx + 1) as i32) * cell).clamp(xs + 1, img_w as i32);
            let mut sum = [0.0_f64; 3];
            let mut cnt = 0_u64;
            let mut sy = ys;
            while sy < ye {
                let mut sx = xs;
                while sx < xe {
                    let idx = ((sy as usize) * (img_w as usize) + (sx as usize)) * 4;
                    if idx + 2 < rgba.len() {
                        sum[0] += f64::from(rgba[idx]);
                        sum[1] += f64::from(rgba[idx + 1]);
                        sum[2] += f64::from(rgba[idx + 2]);
                        cnt += 1;
                    }
                    sx += 2;
                }
                sy += 2;
            }
            if cnt > 0 {
                let n = cnt as f64;
                raw_grid[gy * gw + gx] = [
                    (((sum[0] / n) as f32) * 0.5 + 12.0 * 0.5) / 255.0,
                    (((sum[1] / n) as f32) * 0.5 + 14.0 * 0.5) / 255.0,
                    (((sum[2] / n) as f32) * 0.5 + 18.0 * 0.5) / 255.0,
                ];
            }
        }
    }

    let mut smooth = vec![[0.0_f32; 3]; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let mut acc = [0.0_f32; 3];
            let mut cnt = 0.0_f32;
            for dy in -1_isize..=1 {
                let ny = (gy as isize) + dy;
                if ny >= 0 && ny < (gh as isize) {
                    for dx in -1_isize..=1 {
                        let nx = (gx as isize) + dx;
                        if nx >= 0 && nx < (gw as isize) {
                            let c = raw_grid[(ny as usize) * gw + (nx as usize)];
                            acc[0] += c[0];
                            acc[1] += c[1];
                            acc[2] += c[2];
                            cnt += 1.0;
                        }
                    }
                }
            }
            smooth[gy * gw + gx] = [acc[0] / cnt, acc[1] / cnt, acc[2] / cnt];
        }
    }
    (smooth, gw, gh)
}

/// Hash determinístico em `[0.0, 1.0)` para prévia do frosted glass no canvas Iced.
/// Input: coordenadas `(gx, gy)` | Output: `f32` em `[0.0, 1.0)`.
fn iced_frosted_hash01(gx: i32, gy: i32) -> f32 {
    let mut hv = (gx as u32).wrapping_mul(374_761_393) ^ (gy as u32).wrapping_mul(668_265_263);
    hv = (hv ^ (hv >> 13)).wrapping_mul(1_274_126_177);
    hv ^= hv >> 16;
    (hv as f32) / ((u32::MAX as f32) + 1.0)
}

/// Desenha uma única operação vetorial `AnnotationOp` (caneta, marca-texto, spray, curva, forma ou tarja) sobre o frame do canvas.
/// Input: `frame` (`&mut Frame`), `fit` (`Rectangle`), `scale` (`f32`), `op` (`&AnnotationOp`), `base_info` opcional | Output: `()`
fn draw_scene_op(
    frame: &mut Frame,
    fit: Rectangle,
    scale: f32,
    op: &AnnotationOp,
    base_info: Option<(&[u8], u32, u32)>,
) {
    match op {
        AnnotationOp::Pen {
            points,
            color,
            width,
            style,
        } => draw_polyline(
            frame,
            fit,
            scale,
            points,
            to_iced_color(*color, 1.0),
            *width,
            *style,
        ),
        AnnotationOp::Highlighter {
            points,
            color,
            width,
            opacity,
        } => draw_polyline(
            frame,
            fit,
            scale,
            points,
            to_iced_color(*color, *opacity),
            *width,
            LineStyle::Solid,
        ),
        AnnotationOp::Spray {
            points,
            color,
            radius,
        } => {
            let soft_r = (radius * 1.35).max(5.0);
            let spacing = (soft_r * 0.25).clamp(1.5, 6.0);
            let stamps = spray_stamp_points(points, spacing);
            let boost = if stamps.len() == 1 { 2.0_f32 } else { 1.0 };
            let r_px = soft_r * scale;
            const RINGS: [(f32, f32); 4] =
                [(1.0, 0.020), (0.72, 0.040), (0.45, 0.070), (0.22, 0.090)];
            for pt in stamps {
                let c = to_canvas_pt(fit, scale, pt);
                for (rf, alpha) in RINGS {
                    frame.fill(
                        &Path::circle(c, r_px * rf),
                        to_iced_color(*color, alpha * boost),
                    );
                }
            }
        }
        AnnotationOp::CurvedLine {
            points,
            color,
            width,
            style,
        } => {
            let spline = catmull_rom_spline_points(points);
            draw_polyline(
                frame,
                fit,
                scale,
                &spline,
                to_iced_color(*color, 1.0),
                *width,
                *style,
            );
        }
        AnnotationOp::Shape {
            kind,
            start,
            end,
            color,
            width,
            style,
        } => {
            let sw = (width * scale).max(1.0);
            let dash_segments = [0.1, sw * 2.2, sw * 3.0, sw * 2.0];
            let line_dash = match style {
                LineStyle::Solid => LineDash::default(),
                LineStyle::Dotted => LineDash {
                    segments: &dash_segments[0..2],
                    offset: 0,
                },
                LineStyle::Dashed => LineDash {
                    segments: &dash_segments[2..4],
                    offset: 0,
                },
            };
            let stroke = Stroke {
                line_dash,
                ..Stroke::default()
                    .with_color(to_iced_color(*color, 1.0))
                    .with_width(sw)
                    .with_line_cap(LineCap::Round)
                    .with_line_join(LineJoin::Round)
            };
            let solid_stroke = Stroke::default()
                .with_color(to_iced_color(*color, 1.0))
                .with_width(sw)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round);
            let a = to_canvas_pt(fit, scale, *start);
            let b = to_canvas_pt(fit, scale, *end);
            match kind {
                ShapeKind::Rectangle => {
                    let tl = IcedPoint::new(a.x.min(b.x), a.y.min(b.y));
                    let sz = Size::new((a.x - b.x).abs(), (a.y - b.y).abs());
                    frame.stroke(&Path::rectangle(tl, sz), stroke);
                }
                ShapeKind::Ellipse => {
                    let cx = (a.x + b.x) * 0.5;
                    let cy = (a.y + b.y) * 0.5;
                    let rx = ((a.x - b.x).abs() * 0.5).max(0.5);
                    let ry = ((a.y - b.y).abs() * 0.5).max(0.5);
                    let path = Path::new(|builder| {
                        builder.ellipse(canvas::path::arc::Elliptical {
                            center: IcedPoint::new(cx, cy),
                            radii: iced::Vector::new(rx, ry),
                            rotation: iced::Radians(0.0),
                            start_angle: iced::Radians(0.0),
                            end_angle: iced::Radians(std::f32::consts::TAU),
                        });
                    });
                    frame.stroke(&path, stroke);
                }
                ShapeKind::Line => {
                    frame.stroke(&Path::line(a, b), stroke);
                }
                ShapeKind::Arrow | ShapeKind::DoubleArrow => {
                    frame.stroke(&Path::line(a, b), stroke);
                    let dx = b.x - a.x;
                    let dy = b.y - a.y;
                    let len = (dx * dx + dy * dy).sqrt();
                    if len > 1e-3 {
                        let head_len = (width * scale * 3.5).clamp(8.0, 28.0).min(len * 0.48);
                        let angle = dy.atan2(dx);
                        let spread = 0.45_f32;
                        let p1 = IcedPoint::new(
                            b.x - head_len * (angle - spread).cos(),
                            b.y - head_len * (angle - spread).sin(),
                        );
                        let p2 = IcedPoint::new(
                            b.x - head_len * (angle + spread).cos(),
                            b.y - head_len * (angle + spread).sin(),
                        );
                        frame.stroke(&Path::line(b, p1), solid_stroke);
                        frame.stroke(&Path::line(b, p2), solid_stroke);

                        if *kind == ShapeKind::DoubleArrow {
                            let rev = angle + std::f32::consts::PI;
                            let s1 = IcedPoint::new(
                                a.x - head_len * (rev - spread).cos(),
                                a.y - head_len * (rev - spread).sin(),
                            );
                            let s2 = IcedPoint::new(
                                a.x - head_len * (rev + spread).cos(),
                                a.y - head_len * (rev + spread).sin(),
                            );
                            frame.stroke(&Path::line(a, s1), solid_stroke);
                            frame.stroke(&Path::line(a, s2), solid_stroke);
                        }
                    }
                }
            }
        }
        AnnotationOp::RedactBox { rect } => {
            let r = rect.normalized();
            if r.width <= 0.0 || r.height <= 0.0 {
                return;
            }
            let (ix0, iy0, ix1, iy1, grid, gw, gh, cell) = if let Some((rgba, iw, ih)) = base_info {
                let x0 = r.x.floor().clamp(0.0, iw as f32) as i32;
                let y0 = r.y.floor().clamp(0.0, ih as f32) as i32;
                let x1 = (r.x + r.width).ceil().clamp(0.0, iw as f32) as i32;
                let y1 = (r.y + r.height).ceil().clamp(0.0, ih as f32) as i32;
                let min_dim = ((x1 - x0).min(y1 - y0)).max(1) as f32;
                let cell = (min_dim * 0.25).round().clamp(4.0, 12.0) as i32;
                let (g, gw, gh) = build_preview_smoked_grid(rgba, iw, ih, x0, y0, x1, y1, cell);
                (x0, y0, x1, y1, g, gw, gh, cell)
            } else {
                (
                    r.x.floor() as i32,
                    r.y.floor() as i32,
                    (r.x + r.width).ceil() as i32,
                    (r.y + r.height).ceil() as i32,
                    vec![[30.0 / 255.0, 32.0 / 255.0, 36.0 / 255.0]],
                    1usize,
                    1usize,
                    8i32,
                )
            };

            let gx0_f = ix0 as f32;
            let gy0_f = iy0 as f32;
            let gx1_f = ix1 as f32;
            let gy1_f = iy1 as f32;
            let scatter_r = ((gx1_f - gx0_f).min(gy1_f - gy0_f) * 0.40).clamp(2.5, 16.0);
            let pad = scatter_r.ceil() as i32;

            // 1. Interior desfocado + camada escura 50% por células com granulação estocástica.
            for gy_idx in 0..gh {
                let cy0 = (iy0 + (gy_idx as i32) * cell).min(iy1);
                let cy1 = (iy0 + ((gy_idx + 1) as i32) * cell).min(iy1);
                if cy1 <= cy0 {
                    continue;
                }
                for gx_idx in 0..gw {
                    let cx0 = (ix0 + (gx_idx as i32) * cell).min(ix1);
                    let cx1 = (ix0 + ((gx_idx + 1) as i32) * cell).min(ix1);
                    if cx1 <= cx0 {
                        continue;
                    }
                    let c = grid[gy_idx * gw + gx_idx];
                    let tl = to_canvas_pt(fit, scale, Point::new(cx0 as f32, cy0 as f32));
                    let br = to_canvas_pt(fit, scale, Point::new(cx1 as f32, cy1 as f32));
                    frame.fill_rectangle(
                        tl,
                        Size::new((br.x - tl.x).max(1.0), (br.y - tl.y).max(1.0)),
                        Color::from_rgb(c[0], c[1], c[2]),
                    );
                }
            }

            let step = (3.0 / scale.max(0.1)).round().clamp(2.0, 5.0) as i32;
            let dot_sz = ((step as f32) * scale).max(1.5);
            let mut gy = iy0 - pad;
            while gy < iy1 + pad {
                let cy = (gy as f32) + 0.5;
                let dy = if cy < gy0_f {
                    gy0_f - cy
                } else if cy > gy1_f {
                    cy - gy1_f
                } else {
                    0.0
                };
                let mut gx = ix0 - pad;
                while gx < ix1 + pad {
                    let h_val = iced_frosted_hash01(gx, gy);
                    let sx_idx = (((gx - ix0) / cell).clamp(0, (gw as i32) - 1)) as usize;
                    let sy_idx = (((gy - iy0) / cell).clamp(0, (gh as i32) - 1)) as usize;
                    let edge_c = grid[sy_idx * gw + sx_idx];

                    if gx >= ix0 && gx < ix1 && gy >= iy0 && gy < iy1 {
                        // Partículas de vidro fosco dentro do retângulo dispersando células vizinhas
                        let jx =
                            ((((h_val * 17.0).fract() * 2.0 - 1.0) * scatter_r).round()) as i32;
                        let jy =
                            ((((h_val * 31.0).fract() * 2.0 - 1.0) * scatter_r).round()) as i32;
                        let nsx = (((gx - ix0 + jx) / cell).clamp(0, (gw as i32) - 1)) as usize;
                        let nsy = (((gy - iy0 + jy) / cell).clamp(0, (gh as i32) - 1)) as usize;
                        let sc = grid[nsy * gw + nsx];
                        let delta = (h_val - 0.5) * 0.08;
                        frame.fill_rectangle(
                            to_canvas_pt(fit, scale, Point::new(gx as f32, gy as f32)),
                            Size::new(dot_sz, dot_sz),
                            Color::from_rgb(
                                (sc[0] + delta).clamp(0.0, 1.0),
                                (sc[1] + delta).clamp(0.0, 1.0),
                                (sc[2] + delta).clamp(0.0, 1.0),
                            ),
                        );
                    } else {
                        let cx = (gx as f32) + 0.5;
                        let dx = if cx < gx0_f {
                            gx0_f - cx
                        } else if cx > gx1_f {
                            cx - gx1_f
                        } else {
                            0.0
                        };
                        let dist = (dx * dx + dy * dy).sqrt();
                        let t = (1.0 - dist / scatter_r).clamp(0.0, 1.0);
                        if t > 0.0 {
                            let prob = t.powf(1.55);
                            if h_val < prob {
                                let alpha =
                                    (0.72 + 0.28 * ((prob - h_val) / prob).sqrt()).clamp(0.0, 1.0);
                                frame.fill_rectangle(
                                    to_canvas_pt(fit, scale, Point::new(gx as f32, gy as f32)),
                                    Size::new(dot_sz, dot_sz),
                                    Color::from_rgba(edge_c[0], edge_c[1], edge_c[2], alpha),
                                );
                            }
                        }
                    }
                    gx += step;
                }
                gy += step;
            }
        }
    }
}

/// Traça uma polilinha suave (ou círculo quando há apenas um ponto) no frame do canvas com estilo de linha configurável.
/// Input: `frame` (`&mut Frame`), `fit` (`Rectangle`), `scale` (`f32`), `points` (`&[Point]`), `color` (`Color`), `width` (`f32`), `style` (`LineStyle`) | Output: `()`
fn draw_polyline(
    frame: &mut Frame,
    fit: Rectangle,
    scale: f32,
    points: &[Point],
    color: Color,
    width: f32,
    style: LineStyle,
) {
    if points.is_empty() {
        return;
    }
    if points.len() == 1 {
        let c = to_canvas_pt(fit, scale, points[0]);
        let r = (width * scale * 0.5).max(1.0);
        frame.fill(&Path::circle(c, r), color);
        return;
    }
    let path = Path::new(|builder| {
        builder.move_to(to_canvas_pt(fit, scale, points[0]));
        for pt in &points[1..] {
            builder.line_to(to_canvas_pt(fit, scale, *pt));
        }
    });
    let sw = (width * scale).max(1.0);
    let dash_segments = [0.1, sw * 2.2, sw * 3.0, sw * 2.0];
    let line_dash = match style {
        LineStyle::Solid => LineDash::default(),
        LineStyle::Dotted => LineDash {
            segments: &dash_segments[0..2],
            offset: 0,
        },
        LineStyle::Dashed => LineDash {
            segments: &dash_segments[2..4],
            offset: 0,
        },
    };
    let stroke = Stroke {
        line_dash,
        ..Stroke::default()
            .with_color(color)
            .with_width(sw)
            .with_line_cap(LineCap::Round)
            .with_line_join(LineJoin::Round)
    };
    frame.stroke(&path, stroke);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_redact_and_copy_produces_png() {
        let base_rgba = vec![255u8; 8 * 8 * 4];
        let mut ed = EditorState::new(base_rgba, 8, 8);
        ed.update(EditorMessage::SelectTool(EditorTool::Redact));
        ed.update(EditorMessage::PointerDown(Point::new(1.0, 1.0)));
        ed.update(EditorMessage::PointerUp(Point::new(5.0, 5.0)));
        assert!(ed.scene.can_undo());

        let action = ed.update(EditorMessage::CopyAndExit);
        match action {
            Some(EditorAction::CopyAndExit(bytes)) => {
                assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
            }
            other => panic!("expected CopyAndExit, got {other:?}"),
        }
    }

    #[test]
    fn editor_supports_custom_colors_up_to_three_and_line_styles() {
        let base_rgba = vec![255u8; 16 * 16 * 4];
        let mut ed = EditorState::new(base_rgba, 16, 16);

        ed.update(EditorMessage::SelectLineStyle(LineStyle::Dotted));
        assert_eq!(ed.line_style, LineStyle::Dotted);

        for slot in 0..MAX_CUSTOM_COLORS {
            let c = SceneColor::rgb((slot as u8) * 40 + 10, 100, 200);
            let act = ed.update(EditorMessage::RequestCustomColor(slot));
            assert!(matches!(act, Some(EditorAction::PickCustomColor { .. })));
            ed.update(EditorMessage::CustomColorPicked(slot, Some(c)));
            assert_eq!(ed.active_color, c);
        }
        assert_eq!(ed.custom_colors.len(), MAX_CUSTOM_COLORS);
        assert!(ed
            .update(EditorMessage::RequestCustomColor(MAX_CUSTOM_COLORS))
            .is_none());

        ed.update(EditorMessage::SelectTool(EditorTool::Spray));
        ed.update(EditorMessage::PointerDown(Point::new(4.0, 4.0)));
        ed.update(EditorMessage::PointerMove(Point::new(8.0, 8.0)));
        ed.update(EditorMessage::PointerUp(Point::new(8.0, 8.0)));
        assert_eq!(ed.scene.ops().len(), 1);
    }

    #[test]
    fn editor_redact_smoked_frosted_glass_obfuscates_high_contrast_text() {
        let mut rgba = vec![255u8; 48 * 32 * 4];
        // Left half [0..24]: alternating high-contrast 2px black stripes on white; right half [24..48]: dark blue
        for y in 0..32usize {
            for x in 0..48usize {
                let idx = (y * 48 + x) * 4;
                if x < 24 && (x / 2).is_multiple_of(2) {
                    rgba[idx..idx + 4].copy_from_slice(&[0, 0, 0, 255]);
                } else if x >= 24 {
                    rgba[idx..idx + 4].copy_from_slice(&[20, 24, 48, 255]);
                }
            }
        }

        let mut ed = EditorState::new(rgba, 48, 32);
        ed.update(EditorMessage::SelectTool(EditorTool::Redact));
        ed.update(EditorMessage::PointerDown(Point::new(4.2, 4.3)));
        ed.update(EditorMessage::PointerUp(Point::new(43.7, 27.8)));

        let png = match ed.update(EditorMessage::CopyAndExit) {
            Some(EditorAction::CopyAndExit(bytes)) => bytes,
            other => panic!("unexpected action {other:?}"),
        };
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        for y in 8..24u32 {
            for x in 8..16u32 {
                let px = decoded.get_pixel(x, y).0;
                assert!(
                    px[0] <= 180 && px[0] >= 25,
                    "high contrast leaked at ({x},{y}): {px:?}"
                );
            }
        }
        let avg_x_range = |x_start: u32, x_end: u32| -> f64 {
            let mut sum = 0.0_f64;
            let mut cnt = 0_u64;
            for y in 12..20u32 {
                for x in x_start..x_end {
                    sum += f64::from(decoded.get_pixel(x, y).0[0]);
                    cnt += 1;
                }
            }
            sum / (cnt as f64)
        };
        let avg_left = avg_x_range(8, 18);
        let avg_right = avg_x_range(28, 40);
        assert!(
            avg_left > avg_right + 25.0,
            "avg_left={avg_left:.1}, avg_right={avg_right:.1}"
        );
    }

    #[test]
    fn editor_curved_line_supports_multi_click_waypoints_and_freehand_drag() {
        let base_rgba = vec![255u8; 64 * 64 * 4];
        let mut ed = EditorState::new(base_rgba, 64, 64);
        ed.update(EditorMessage::SelectTool(EditorTool::CurvedLine));

        // 1. Multi-click waypoints (click-by-click without drag >= 8px)
        for pt in [
            Point::new(8.0, 20.0),
            Point::new(24.0, 8.0),
            Point::new(48.0, 24.0),
        ] {
            ed.update(EditorMessage::PointerDown(pt));
            ed.update(EditorMessage::PointerUp(pt));
        }
        assert_eq!(ed.curve_waypoints.len(), 3);
        assert!(ed.scene.ops().is_empty());

        // Right-click / FinishCurve commits the spline op
        ed.update(EditorMessage::FinishCurve);
        assert!(ed.curve_waypoints.is_empty());
        assert_eq!(ed.scene.ops().len(), 1);
        assert!(matches!(
            &ed.scene.ops()[0],
            AnnotationOp::CurvedLine { points, .. } if points.len() == 3
        ));

        // 2. Freehand drag (>= 8px) commits on pointer release immediately
        ed.update(EditorMessage::PointerDown(Point::new(6.0, 40.0)));
        ed.update(EditorMessage::PointerMove(Point::new(20.0, 32.0)));
        ed.update(EditorMessage::PointerMove(Point::new(38.0, 46.0)));
        ed.update(EditorMessage::PointerUp(Point::new(38.0, 46.0)));
        assert!(ed.curve_waypoints.is_empty());
        assert_eq!(ed.scene.ops().len(), 2);
    }

    #[test]
    fn editor_visual_crop_updates_viewport_bounds_maps_points_and_reverts_cleanly() {
        let base_rgba = vec![255u8; 100 * 80 * 4];
        let mut ed = EditorState::new(base_rgba, 100, 80);
        assert_eq!(ed.visible_crop_bounds(), (0, 0, 100, 80, false));

        // 1. First crop updates visible bounds and canvas fit size immediately.
        ed.update(EditorMessage::SelectTool(EditorTool::Crop));
        ed.update(EditorMessage::PointerDown(Point::new(20.2, 10.4)));
        ed.update(EditorMessage::PointerUp(Point::new(69.6, 49.3)));
        assert_eq!(ed.visible_crop_bounds(), (20, 10, 50, 40, true));

        let canvas = EditorCanvas { state: &ed };
        let viewport = Rectangle::new(IcedPoint::new(0.0, 0.0), Size::new(400.0, 300.0));
        let fit = canvas.fit_rect(viewport);
        assert_eq!(fit.width, 50.0);
        assert_eq!(fit.height, 40.0);
        assert!((fit.x - 175.0).abs() < 0.01);
        assert!((fit.y - 130.0).abs() < 0.01);

        let mapped_tl = canvas.to_image_point(fit, IcedPoint::new(fit.x, fit.y));
        assert!((mapped_tl.x - 20.0).abs() < 0.01);
        assert!((mapped_tl.y - 10.0).abs() < 0.01);

        let mapped_br =
            canvas.to_image_point(fit, IcedPoint::new(fit.x + fit.width, fit.y + fit.height));
        assert!((mapped_br.x - 70.0).abs() < 0.01);
        assert!((mapped_br.y - 50.0).abs() < 0.01);

        // 2. Chained second crop inside the already cropped viewport narrows further & exports cleanly.
        ed.update(EditorMessage::PointerDown(Point::new(30.0, 15.0)));
        ed.update(EditorMessage::PointerUp(Point::new(55.0, 35.0)));
        assert_eq!(ed.visible_crop_bounds(), (30, 15, 25, 20, true));

        let png = match ed.update(EditorMessage::CopyAndExit) {
            Some(EditorAction::CopyAndExit(bytes)) => bytes,
            other => panic!("expected CopyAndExit, got {other:?}"),
        };
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (25, 20));

        // 3. Undo and ResetCrop remain non-destructive and restore previous viewports cleanly.
        ed.update(EditorMessage::Undo);
        assert_eq!(ed.visible_crop_bounds(), (20, 10, 50, 40, true));

        ed.update(EditorMessage::ResetCrop);
        assert_eq!(ed.visible_crop_bounds(), (0, 0, 100, 80, false));
    }
}
