//! Annotation editor window backed by `lx_snip_core::scene::AnnotationScene`
//! and `lx_snip_core::export::render_and_export`.

use std::path::PathBuf;

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, LineJoin, Path, Stroke};
use iced::widget::{button, container, mouse_area, row, text, Column};
use iced::{event, Alignment, Color, Element, Length, Point as IcedPoint, Rectangle, Size};
use lx_snip_core::export::{render_and_export, ExportFormat, DEFAULT_JPEG_QUALITY};
use lx_snip_core::scene::{
    AnnotationOp, AnnotationScene, Color as SceneColor, Point, Rect, ShapeKind,
};

use crate::icons;

/// Visual theme for the editor (Windows 11 Fluent Light / Dark).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTheme {
    Light,
    Dark,
}

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

/// Active drawing/editing tool in the annotation editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTool {
    /// Freehand opaque pen.
    Pen,
    /// Translucent highlighter.
    Highlighter,
    /// Stroke-level eraser.
    Eraser,
    /// Geometric shape tool.
    Shape(ShapeKind),
    /// Destructive solid-black redaction box.
    Redact,
    /// Crop rectangle tool.
    Crop,
}

/// Messages emitted by the annotation editor.
#[derive(Debug, Clone)]
pub enum EditorMessage {
    /// Switch active annotation tool.
    SelectTool(EditorTool),
    /// Switch active stroke color.
    SelectColor(SceneColor),
    /// Set stroke width in pixels.
    SetStrokeWidth(f32),
    /// Pointer pressed in image-pixel coordinates.
    PointerDown(Point),
    /// Pointer moved in image-pixel coordinates.
    PointerMove(Point),
    /// Pointer released in image-pixel coordinates.
    PointerUp(Point),
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
    /// Minimize the editor window.
    MinimizeWindow,
    /// Toggle maximize on the editor window.
    ToggleMaximizeWindow,
    /// Start dragging the editor window from the titlebar.
    DragWindow,
    /// Close the editor window.
    CloseWindow,
}

/// Outcome returned by [`EditorState::update`] when the editor triggers an external action.
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
    /// Start a new screen capture.
    NewCapture,
    /// Open Snip History window.
    OpenHistory,
    /// Minimize the editor window.
    Minimize,
    /// Toggle maximize on the editor window.
    ToggleMaximize,
    /// Drag the editor window.
    DragWindow,
    /// Close the editor window.
    Close,
}

/// State of the annotation editor window.
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
    /// Active stroke color.
    pub active_color: SceneColor,
    /// Active stroke width in image pixels.
    pub stroke_width: f32,
    /// Active freehand stroke points.
    pub in_progress_points: Vec<Point>,
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
    /// Creates a new [`EditorState`] from a straight RGBA8 image.
    pub fn new(base_rgba: Vec<u8>, width: u32, height: u32) -> Self {
        let bg_handle = iced::widget::image::Handle::from_rgba(width, height, base_rgba.clone());
        Self {
            base_rgba,
            width,
            height,
            bg_handle,
            scene: AnnotationScene::new(width, height),
            active_tool: EditorTool::Pen,
            active_color: SceneColor::RED,
            stroke_width: 4.0,
            in_progress_points: Vec::new(),
            drag_start: None,
            drag_current: None,
            status_text: None,
            theme: UiTheme::Light,
        }
    }

    /// Loads an [`EditorState`] from a PNG file on disk.
    pub fn from_png_file(path: &std::path::Path) -> Result<Self, String> {
        let dyn_img =
            image::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        let rgba_img = dyn_img.to_rgba8();
        let (w, h) = rgba_img.dimensions();
        Ok(Self::new(rgba_img.into_raw(), w, h))
    }

    /// Updates editor state and optionally returns an [`EditorAction`].
    pub fn update(&mut self, msg: EditorMessage) -> Option<EditorAction> {
        match msg {
            EditorMessage::SelectTool(tool) => {
                self.active_tool = tool;
                self.in_progress_points.clear();
                self.drag_start = None;
                self.drag_current = None;
                None
            }
            EditorMessage::SelectColor(color) => {
                self.active_color = color;
                None
            }
            EditorMessage::SetStrokeWidth(w) => {
                self.stroke_width = w.clamp(1.0, 48.0);
                None
            }
            EditorMessage::PointerDown(pt) => {
                match self.active_tool {
                    EditorTool::Pen | EditorTool::Highlighter => {
                        self.in_progress_points.clear();
                        self.in_progress_points.push(pt);
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
                    EditorTool::Pen | EditorTool::Highlighter => {
                        if !self.in_progress_points.is_empty() {
                            self.in_progress_points.push(pt);
                        }
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
            EditorMessage::PointerUp(pt) => {
                match self.active_tool {
                    EditorTool::Pen => {
                        if !self.in_progress_points.is_empty() {
                            let points = std::mem::take(&mut self.in_progress_points);
                            self.scene.push_op(AnnotationOp::Pen {
                                points,
                                color: self.active_color,
                                width: self.stroke_width,
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
                            }
                        }
                    }
                }
                None
            }
            EditorMessage::Undo => {
                self.scene.undo();
                None
            }
            EditorMessage::Redo => {
                self.scene.redo();
                None
            }
            EditorMessage::ResetCrop => {
                self.scene.set_crop(None);
                None
            }
            EditorMessage::CopyAndExit => {
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
            EditorMessage::SaveAs => Some(EditorAction::RunSaveAs {
                base_rgba: self.base_rgba.clone(),
                width: self.width,
                height: self.height,
                scene: self.scene.clone(),
            }),
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
            EditorMessage::NewCapture => Some(EditorAction::NewCapture),
            EditorMessage::OpenHistory => Some(EditorAction::OpenHistory),
            EditorMessage::ToggleTheme => {
                self.theme = match self.theme {
                    UiTheme::Light => UiTheme::Dark,
                    UiTheme::Dark => UiTheme::Light,
                };
                None
            }
            EditorMessage::MinimizeWindow => Some(EditorAction::Minimize),
            EditorMessage::ToggleMaximizeWindow => Some(EditorAction::ToggleMaximize),
            EditorMessage::DragWindow => Some(EditorAction::DragWindow),
            EditorMessage::CloseWindow => Some(EditorAction::Close),
        }
    }

    /// Renders the annotation editor UI matching Windows 11 Snipping Tool (Light & Dark themes).
    pub fn view(&self) -> Element<'_, EditorMessage> {
        let p = self.theme.palette();

        // Row 0: Windows 11 Integrated Title Bar ("Lx Snipping Tool" + Minimize / Maximize / Close)
        let title_drag = mouse_area(
            row![
                icons::app_icon(18.0),
                text("Lx Snipping Tool").size(12).color(p.text),
                iced::widget::Space::with_width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([6, 12]),
        )
        .on_press(EditorMessage::DragWindow);

        let win_controls = row![
            button(icons::icon(icons::ICON_MINIMIZE, 12.0, p.text))
                .padding([7, 14])
                .style(win11_flat_btn(p, false))
                .on_press(EditorMessage::MinimizeWindow),
            button(icons::icon(icons::ICON_MAXIMIZE, 12.0, p.text))
                .padding([7, 14])
                .style(win11_flat_btn(p, false))
                .on_press(EditorMessage::ToggleMaximizeWindow),
            button(icons::icon(icons::ICON_CLOSE, 12.0, p.text))
                .padding([7, 14])
                .style(win11_close_btn(p))
                .on_press(EditorMessage::CloseWindow),
        ]
        .spacing(0)
        .align_y(Alignment::Center);

        let title_bar = container(row![title_drag, win_controls].align_y(Alignment::Center))
            .width(Length::Fill)
            .style(move |_theme| container::Style {
                background: Some(iced::Background::Color(p.header_bg)),
                ..Default::default()
            });

        // Row 1: Windows 11 Header Bar (+ New | Undo Redo | Save Copy History Theme)
        let new_btn = button(
            row![
                text("+").size(16).color(p.accent),
                text("New").size(13).color(p.text),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 14])
        .style(win11_card_btn(p))
        .on_press(EditorMessage::NewCapture);

        let mut undo_btn = button(icons::icon(icons::ICON_UNDO, 15.0, p.text))
            .padding([6, 10])
            .style(win11_flat_btn(p, false));
        if self.scene.can_undo() {
            undo_btn = undo_btn.on_press(EditorMessage::Undo);
        }

        let mut redo_btn = button(icons::icon(icons::ICON_REDO, 15.0, p.text))
            .padding([6, 10])
            .style(win11_flat_btn(p, false));
        if self.scene.can_redo() {
            redo_btn = redo_btn.on_press(EditorMessage::Redo);
        }

        let save_btn = button(
            row![
                icons::icon(icons::ICON_SAVE, 14.0, p.text),
                text("Save").size(13),
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

        let history_btn = button(
            row![
                icons::icon(icons::ICON_HISTORY, 14.0, p.text),
                text("History").size(12),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding([6, 10])
        .style(win11_flat_btn(p, false))
        .on_press(EditorMessage::OpenHistory);

        let theme_label = match self.theme {
            UiTheme::Light => "Dark",
            UiTheme::Dark => "Light",
        };
        let theme_btn = button(text(theme_label).size(12))
            .padding([6, 10])
            .style(win11_flat_btn(p, false))
            .on_press(EditorMessage::ToggleTheme);

        let header_bar = container(
            row![
                new_btn,
                v_divider(p),
                undo_btn,
                redo_btn,
                iced::widget::Space::with_width(Length::Fill),
                save_btn,
                copy_btn,
                v_divider(p),
                history_btn,
                theme_btn,
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([6, 14])
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(p.header_bg)),
            border: iced::Border {
                color: p.card_border,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        });

        // Row 2: Centered Floating Annotation Pill with crisp SVG icons
        let tools_pill = container(
            row![
                icon_tool_btn(
                    p,
                    icons::ICON_PEN,
                    self.active_tool == EditorTool::Pen,
                    EditorTool::Pen
                ),
                icon_tool_btn(
                    p,
                    icons::ICON_HIGHLIGHTER,
                    self.active_tool == EditorTool::Highlighter,
                    EditorTool::Highlighter
                ),
                icon_tool_btn(
                    p,
                    icons::ICON_ERASER,
                    self.active_tool == EditorTool::Eraser,
                    EditorTool::Eraser
                ),
                v_divider(p),
                icon_tool_btn(
                    p,
                    icons::ICON_LINE,
                    self.active_tool == EditorTool::Shape(ShapeKind::Line),
                    EditorTool::Shape(ShapeKind::Line)
                ),
                icon_tool_btn(
                    p,
                    icons::ICON_RECT,
                    self.active_tool == EditorTool::Shape(ShapeKind::Rectangle),
                    EditorTool::Shape(ShapeKind::Rectangle)
                ),
                icon_tool_btn(
                    p,
                    icons::ICON_ELLIPSE,
                    self.active_tool == EditorTool::Shape(ShapeKind::Ellipse),
                    EditorTool::Shape(ShapeKind::Ellipse)
                ),
                icon_tool_btn(
                    p,
                    icons::ICON_ARROW,
                    self.active_tool == EditorTool::Shape(ShapeKind::Arrow),
                    EditorTool::Shape(ShapeKind::Arrow)
                ),
                v_divider(p),
                icon_tool_btn(
                    p,
                    icons::ICON_REDACT,
                    self.active_tool == EditorTool::Redact,
                    EditorTool::Redact
                ),
                icon_tool_btn(
                    p,
                    icons::ICON_CROP,
                    self.active_tool == EditorTool::Crop,
                    EditorTool::Crop
                ),
                v_divider(p),
                color_swatch(p, SceneColor::BLACK, self.active_color == SceneColor::BLACK),
                color_swatch(p, SceneColor::RED, self.active_color == SceneColor::RED),
                color_swatch(
                    p,
                    SceneColor::YELLOW,
                    self.active_color == SceneColor::YELLOW
                ),
                color_swatch(
                    p,
                    SceneColor::rgb(37, 99, 235),
                    self.active_color == SceneColor::rgb(37, 99, 235)
                ),
                color_swatch(
                    p,
                    SceneColor::rgb(34, 197, 94),
                    self.active_color == SceneColor::rgb(34, 197, 94)
                ),
                color_swatch(p, SceneColor::WHITE, self.active_color == SceneColor::WHITE),
                v_divider(p),
                width_btn(p, "S", 2.0, (self.stroke_width - 2.0).abs() < 0.1),
                width_btn(p, "M", 4.0, (self.stroke_width - 4.0).abs() < 0.1),
                width_btn(p, "L", 8.0, (self.stroke_width - 8.0).abs() < 0.1),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([5, 10])
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(p.card_bg)),
            border: iced::Border {
                color: p.card_border,
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });

        let mut sub_row = row![tools_pill].spacing(8).align_y(Alignment::Center);
        if self.scene.crop_rect.is_some() {
            sub_row = sub_row.push(
                button(text("Reset Crop").size(12))
                    .padding([6, 10])
                    .style(win11_card_btn(p))
                    .on_press(EditorMessage::ResetCrop),
            );
        }

        let sub_bar = container(sub_row)
            .width(Length::Fill)
            .padding([8, 12])
            .center_x(Length::Fill)
            .style(move |_theme| container::Style {
                background: Some(iced::Background::Color(p.workspace_bg)),
                ..Default::default()
            });

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

        let dim_label = format!("{} × {} px", self.width, self.height);
        let status_msg = self.status_text.as_deref().unwrap_or("Ready");
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
            .push(title_bar)
            .push(header_bar)
            .push(sub_bar)
            .push(workspace)
            .push(status_bar);

        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

/// Runs the native `ashpd::desktop::file_chooser` Save As dialog and writes the exported
/// PNG or JPEG (determined by file extension, defaulting to PNG) to disk.
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

fn v_divider(p: ThemePalette) -> Element<'static, EditorMessage> {
    container(iced::widget::Space::with_width(1).height(20))
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(p.card_border)),
            ..Default::default()
        })
        .into()
}

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

fn win11_close_btn(
    p: ThemePalette,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style + Copy {
    move |_theme, status| {
        let (bg, fg) = match status {
            button::Status::Hovered | button::Status::Pressed => {
                (Color::from_rgb8(196, 43, 28), Color::WHITE)
            }
            _ => (Color::TRANSPARENT, p.text),
        };
        button::Style {
            background: Some(iced::Background::Color(bg)),
            text_color: fg,
            border: iced::Border::default(),
            ..Default::default()
        }
    }
}

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

fn width_btn(
    p: ThemePalette,
    label: &str,
    width: f32,
    active: bool,
) -> button::Button<'_, EditorMessage> {
    button(text(label).size(12))
        .padding([4, 8])
        .style(win11_flat_btn(p, active))
        .on_press(EditorMessage::SetStrokeWidth(width))
}

struct EditorCanvas<'a> {
    state: &'a EditorState,
}

impl<'a> EditorCanvas<'a> {
    fn fit_rect(&self, bounds: Rectangle) -> Rectangle {
        let img_w = self.state.width.max(1) as f32;
        let img_h = self.state.height.max(1) as f32;
        let pad = 24.0;
        let avail_w = (bounds.width - pad).max(1.0);
        let avail_h = (bounds.height - pad).max(1.0);
        let scale = (avail_w / img_w).min(avail_h / img_h).min(1.0);
        let draw_w = img_w * scale;
        let draw_h = img_h * scale;
        Rectangle {
            x: (bounds.width - draw_w) * 0.5,
            y: (bounds.height - draw_h) * 0.5,
            width: draw_w,
            height: draw_h,
        }
    }

    fn to_image_point(&self, fit: Rectangle, pos: IcedPoint) -> Point {
        let x = ((pos.x - fit.x) / fit.width.max(1.0)) * (self.state.width as f32);
        let y = ((pos.y - fit.y) / fit.height.max(1.0)) * (self.state.height as f32);
        Point::new(
            x.clamp(0.0, self.state.width as f32),
            y.clamp(0.0, self.state.height as f32),
        )
    }

    fn to_canvas_point(&self, fit: Rectangle, pt: Point) -> IcedPoint {
        let sx = fit.width / (self.state.width.max(1) as f32);
        let sy = fit.height / (self.state.height.max(1) as f32);
        IcedPoint::new(fit.x + pt.x * sx, fit.y + pt.y * sy)
    }
}

impl<'a> canvas::Program<EditorMessage> for EditorCanvas<'a> {
    type State = bool;

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
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) if *is_dragging => (
                event::Status::Captured,
                Some(EditorMessage::PointerMove(img_pt)),
            ),
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
        let scale = fit.width / (self.state.width.max(1) as f32);

        for op in self.state.scene.ops() {
            draw_scene_op(&mut frame, fit, scale, op);
        }

        // In-progress stroke/shape/redact/crop preview.
        match self.state.active_tool {
            EditorTool::Pen if self.state.in_progress_points.len() >= 2 => {
                let op = AnnotationOp::Pen {
                    points: self.state.in_progress_points.clone(),
                    color: self.state.active_color,
                    width: self.state.stroke_width,
                };
                draw_scene_op(&mut frame, fit, scale, &op);
            }
            EditorTool::Highlighter if self.state.in_progress_points.len() >= 2 => {
                let op = AnnotationOp::Highlighter {
                    points: self.state.in_progress_points.clone(),
                    color: self.state.active_color,
                    width: (self.state.stroke_width * 3.0).max(12.0),
                    opacity: 0.4,
                };
                draw_scene_op(&mut frame, fit, scale, &op);
            }
            EditorTool::Shape(kind) => {
                if let (Some(start), Some(end)) = (self.state.drag_start, self.state.drag_current) {
                    let op = AnnotationOp::Shape {
                        kind,
                        start,
                        end,
                        color: self.state.active_color,
                        width: self.state.stroke_width,
                    };
                    draw_scene_op(&mut frame, fit, scale, &op);
                }
            }
            EditorTool::Redact => {
                if let (Some(start), Some(end)) = (self.state.drag_start, self.state.drag_current) {
                    let op = AnnotationOp::RedactBox {
                        rect: Rect::new(start.x, start.y, end.x - start.x, end.y - start.y)
                            .normalized(),
                    };
                    draw_scene_op(&mut frame, fit, scale, &op);
                }
            }
            EditorTool::Crop => {
                if let (Some(start), Some(end)) = (self.state.drag_start, self.state.drag_current) {
                    let r =
                        Rect::new(start.x, start.y, end.x - start.x, end.y - start.y).normalized();
                    let tl = self.to_canvas_point(fit, Point::new(r.x, r.y));
                    let p = Path::rectangle(tl, Size::new(r.width * scale, r.height * scale));
                    frame.stroke(
                        &p,
                        Stroke::default()
                            .with_color(Color::from_rgb8(96, 165, 250))
                            .with_width(2.0),
                    );
                }
            }
            _ => {}
        }

        if let Some(crop) = self.state.scene.crop_rect {
            let tl = self.to_canvas_point(fit, Point::new(crop.x, crop.y));
            let p = Path::rectangle(tl, Size::new(crop.width * scale, crop.height * scale));
            frame.stroke(
                &p,
                Stroke::default()
                    .with_color(Color::from_rgb8(56, 189, 248))
                    .with_width(2.0),
            );
        }

        vec![frame.into_geometry()]
    }
}

fn to_iced_color(c: SceneColor, alpha_mul: f32) -> Color {
    Color::from_rgba8(
        c.r,
        c.g,
        c.b,
        ((f32::from(c.a) / 255.0) * alpha_mul).clamp(0.0, 1.0),
    )
}

fn to_canvas_pt(fit: Rectangle, scale: f32, pt: Point) -> IcedPoint {
    IcedPoint::new(fit.x + pt.x * scale, fit.y + pt.y * scale)
}

fn draw_scene_op(frame: &mut Frame, fit: Rectangle, scale: f32, op: &AnnotationOp) {
    match op {
        AnnotationOp::Pen {
            points,
            color,
            width,
        } => draw_polyline(
            frame,
            fit,
            scale,
            points,
            to_iced_color(*color, 1.0),
            *width,
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
        ),
        AnnotationOp::Shape {
            kind,
            start,
            end,
            color,
            width,
        } => {
            let stroke = Stroke::default()
                .with_color(to_iced_color(*color, 1.0))
                .with_width((width * scale).max(1.0))
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
                ShapeKind::Arrow => {
                    frame.stroke(&Path::line(a, b), stroke);
                    let dx = b.x - a.x;
                    let dy = b.y - a.y;
                    let len = (dx * dx + dy * dy).sqrt();
                    if len > 1e-3 {
                        let head_len = (width * scale * 3.5).clamp(8.0, 28.0);
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
                        frame.stroke(&Path::line(b, p1), stroke);
                        frame.stroke(&Path::line(b, p2), stroke);
                    }
                }
            }
        }
        AnnotationOp::RedactBox { rect } => {
            let r = rect.normalized();
            let tl = to_canvas_pt(fit, scale, Point::new(r.x, r.y));
            frame.fill_rectangle(
                tl,
                Size::new(r.width * scale, r.height * scale),
                Color::BLACK,
            );
        }
    }
}

fn draw_polyline(
    frame: &mut Frame,
    fit: Rectangle,
    scale: f32,
    points: &[Point],
    color: Color,
    width: f32,
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
    let stroke = Stroke::default()
        .with_color(color)
        .with_width((width * scale).max(1.0))
        .with_line_cap(LineCap::Round)
        .with_line_join(LineJoin::Round);
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
}
