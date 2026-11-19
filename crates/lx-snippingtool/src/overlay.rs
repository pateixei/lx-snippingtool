//! Multi-monitor freeze-frame selection overlay with cached static background,
//! floating mode pill toolbar, and rectangular/freeform/window/fullscreen selection.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{button, container, image, row, stack, text, Column, Container};
use iced::{event, Alignment, Color, Element, Length, Point as IcedPoint, Rectangle, Size};
use lx_snip_core::config::CaptureMode;
use lx_snip_core::platform::{CapturedImage, OutputGeometry};
use lx_snip_core::scene::{Point, Rect};
use tiny_skia::{FillRule, Mask, PathBuilder, Transform};

use crate::capture_x11::X11WindowBounds;
use crate::icons;
use crate::recorder::RecordingTarget;

/// Target selection inside Level 2 of the Record menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordTarget {
    /// Record the whole desktop / monitor (default).
    Fullscreen,
    /// Drag a rectangle on screen and immediately start recording that region.
    Rect,
    /// Pick a single window to record.
    Window,
}

/// Messages emitted by the freeze-frame overlay.
#[derive(Debug, Clone)]
pub enum OverlayMessage {
    /// Switch active capture mode in the Level 1 floating pill toolbar.
    SelectMode(CaptureMode),
    /// Select a Level 2 recording target (`Retângulo`, `Janela`, or `Fullscreen`).
    SelectRecordTarget(RecordTarget),
    /// Trigger the active Level 2 recording target immediately (`Iniciar gravação`).
    StartRecordNow,
    /// Toggle system/desktop audio capture when recording.
    ToggleSystemAudio,
    /// Toggle microphone capture when recording.
    ToggleMic,
    /// Set pre-capture delay in seconds (for subsequent/re-triggered captures).
    SetDelay(u8),
    /// Pointer pressed at logical coordinates within the overlay.
    PointerDown(Point),
    /// Pointer moved to logical coordinates within the overlay.
    PointerMove(Point),
    /// Pointer released at logical coordinates within the overlay.
    PointerUp(Point),
    /// Immediately capture the full desktop screenshot.
    ConfirmFullscreen,
    /// Cancel capture and close the overlay.
    Cancel,
}

/// Action returned by [`OverlayState::update`] when selection completes or cancels.
#[derive(Debug, Clone)]
pub enum OverlayOutcome {
    /// Selection completed: cropped straight RGBA8 buffer + `(width, height)`.
    Captured {
        /// Straight RGBA8 pixel buffer.
        rgba: Vec<u8>,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// User chose `Janela` on Wayland: close overlay and invoke the Wayland portal window picker.
    CaptureWindowPortal,
    /// Target selected for Phase 2 screen recording.
    StartRecording {
        /// Recording target (`Fullscreen`, `Region`, or `Window`).
        target: RecordingTarget,
        /// Whether system/desktop audio capture is enabled.
        with_system_audio: bool,
        /// Whether microphone audio capture is enabled.
        with_mic: bool,
    },
    /// User cancelled the overlay.
    Cancelled,
}

/// State for the freeze-frame selection overlay.
#[derive(Debug, Clone)]
pub struct OverlayState {
    /// Full captured desktop image.
    pub captured: CapturedImage,
    /// Cached static background handle (`None` in live transparent mode).
    pub bg_handle: Option<image::Handle>,
    /// Active selection mode (Level 1).
    pub mode: CaptureMode,
    /// Whether in-overlay window selection is supported (X11 `_NET_CLIENT_LIST`).
    pub supports_window_mode: bool,
    /// Enumerated X11 windows (empty on Wayland).
    pub x11_windows: Vec<X11WindowBounds>,
    /// Currently hovered X11 window rect in image-pixel coordinates.
    pub hovered_window: Option<Rect>,
    /// Active drag start in image-pixel coordinates.
    pub drag_start: Option<Point>,
    /// Active drag current point in image-pixel coordinates.
    pub drag_current: Option<Point>,
    /// Freeform lasso vertices in image-pixel coordinates.
    pub lasso_points: Vec<Point>,
    /// Current delay setting displayed in the mode bar.
    pub delay_secs: u8,
    /// Active Level 2 target when `mode == CaptureMode::Record`.
    pub record_target: RecordTarget,
    /// Whether system/desktop audio capture is toggled on in Record mode.
    pub record_system_audio: bool,
    /// Whether microphone audio capture is toggled on in Record mode.
    pub record_mic: bool,
}

impl OverlayState {
    /// Creates a new [`OverlayState`] from a captured desktop image.
    pub fn new(
        captured: CapturedImage,
        mode: CaptureMode,
        delay_secs: u8,
        supports_window_mode: bool,
        x11_windows: Vec<X11WindowBounds>,
    ) -> Self {
        let bg_handle = Some(image::Handle::from_rgba(
            captured.width,
            captured.height,
            captured.rgba.clone(),
        ));
        Self {
            captured,
            bg_handle,
            mode,
            supports_window_mode,
            x11_windows,
            hovered_window: None,
            drag_start: None,
            drag_current: None,
            lasso_points: Vec::new(),
            delay_secs,
            record_target: RecordTarget::Fullscreen,
            record_system_audio: true,
            record_mic: false,
        }
    }

    fn start_recording_outcome(&self, target: RecordingTarget) -> Option<OverlayOutcome> {
        Some(OverlayOutcome::StartRecording {
            target,
            with_system_audio: self.record_system_audio,
            with_mic: self.record_mic,
        })
    }

    /// Updates overlay state and returns an [`OverlayOutcome`] when the capture finishes or cancels.
    pub fn update(&mut self, msg: OverlayMessage) -> Option<OverlayOutcome> {
        match msg {
            OverlayMessage::SelectMode(mode) => {
                if mode == CaptureMode::Window && !self.supports_window_mode {
                    return Some(OverlayOutcome::CaptureWindowPortal);
                }
                self.mode = mode;
                self.drag_start = None;
                self.drag_current = None;
                self.lasso_points.clear();
                self.hovered_window = None;
                if mode == CaptureMode::Record {
                    self.record_target = RecordTarget::Fullscreen;
                } else if mode == CaptureMode::Fullscreen {
                    return self.finish_fullscreen();
                }
                None
            }
            OverlayMessage::SelectRecordTarget(target) => {
                self.mode = CaptureMode::Record;
                self.record_target = target;
                self.drag_start = None;
                self.drag_current = None;
                self.lasso_points.clear();
                self.hovered_window = None;
                match target {
                    RecordTarget::Fullscreen => {
                        self.start_recording_outcome(RecordingTarget::Fullscreen)
                    }
                    RecordTarget::Window if !self.supports_window_mode => {
                        self.start_recording_outcome(RecordingTarget::Window)
                    }
                    RecordTarget::Rect | RecordTarget::Window => None,
                }
            }
            OverlayMessage::StartRecordNow => {
                let target =
                    if self.record_target == RecordTarget::Window && !self.supports_window_mode {
                        RecordingTarget::Window
                    } else {
                        RecordingTarget::Fullscreen
                    };
                self.start_recording_outcome(target)
            }
            OverlayMessage::ToggleSystemAudio => {
                self.record_system_audio = !self.record_system_audio;
                None
            }
            OverlayMessage::ToggleMic => {
                self.record_mic = !self.record_mic;
                None
            }
            OverlayMessage::SetDelay(secs) => {
                self.delay_secs = secs;
                None
            }
            OverlayMessage::PointerDown(pt) => {
                match self.mode {
                    CaptureMode::Rect => {
                        self.drag_start = Some(pt);
                        self.drag_current = Some(pt);
                    }
                    CaptureMode::Record => match self.record_target {
                        RecordTarget::Rect => {
                            self.drag_start = Some(pt);
                            self.drag_current = Some(pt);
                        }
                        RecordTarget::Window => {
                            if let Some(win_rect) = self.find_window_at(pt) {
                                return self.start_recording_outcome(RecordingTarget::Region {
                                    rect: win_rect,
                                    full_width: self.captured.width,
                                    full_height: self.captured.height,
                                });
                            }
                        }
                        RecordTarget::Fullscreen => {}
                    },
                    CaptureMode::Freeform => {
                        self.lasso_points.clear();
                        self.lasso_points.push(pt);
                        self.drag_start = Some(pt);
                        self.drag_current = Some(pt);
                    }
                    CaptureMode::Window => {
                        if let Some(win_rect) = self.find_window_at(pt) {
                            return self.finish_rect(win_rect, None);
                        }
                    }
                    CaptureMode::Fullscreen => {
                        return self.finish_fullscreen();
                    }
                }
                None
            }
            OverlayMessage::PointerMove(pt) => {
                match self.mode {
                    CaptureMode::Rect => {
                        if self.drag_start.is_some() {
                            self.drag_current = Some(pt);
                        }
                    }
                    CaptureMode::Record => match self.record_target {
                        RecordTarget::Rect => {
                            if self.drag_start.is_some() {
                                self.drag_current = Some(pt);
                            }
                        }
                        RecordTarget::Window => {
                            self.hovered_window = self.find_window_at(pt);
                        }
                        RecordTarget::Fullscreen => {}
                    },
                    CaptureMode::Freeform => {
                        if self.drag_start.is_some() {
                            self.drag_current = Some(pt);
                            if self
                                .lasso_points
                                .last()
                                .is_none_or(|last| dist_sq(*last, pt) >= 4.0)
                            {
                                self.lasso_points.push(pt);
                            }
                        }
                    }
                    CaptureMode::Window => {
                        self.hovered_window = self.find_window_at(pt);
                    }
                    CaptureMode::Fullscreen => {}
                }
                None
            }
            OverlayMessage::PointerUp(pt) => match self.mode {
                CaptureMode::Rect => {
                    let start = self.drag_start.take()?;
                    self.drag_current = None;
                    let rect = Rect::new(start.x, start.y, pt.x - start.x, pt.y - start.y)
                        .normalized()
                        .clamp(self.captured.width as f32, self.captured.height as f32);
                    if rect.width >= 4.0 && rect.height >= 4.0 {
                        self.finish_rect(rect, None)
                    } else {
                        None
                    }
                }
                CaptureMode::Record => {
                    if self.record_target != RecordTarget::Rect {
                        return None;
                    }
                    let start = self.drag_start.take()?;
                    self.drag_current = None;
                    let rect = Rect::new(start.x, start.y, pt.x - start.x, pt.y - start.y)
                        .normalized()
                        .clamp(self.captured.width as f32, self.captured.height as f32);
                    if rect.width >= 4.0 && rect.height >= 4.0 {
                        self.start_recording_outcome(RecordingTarget::Region {
                            rect,
                            full_width: self.captured.width,
                            full_height: self.captured.height,
                        })
                    } else {
                        None
                    }
                }
                CaptureMode::Freeform => {
                    self.drag_start = None;
                    self.drag_current = None;
                    if self.lasso_points.len() < 3 {
                        self.lasso_points.clear();
                        return None;
                    }
                    let pts = std::mem::take(&mut self.lasso_points);
                    let bbox = bounding_box(&pts)
                        .clamp(self.captured.width as f32, self.captured.height as f32);
                    if bbox.width >= 4.0 && bbox.height >= 4.0 {
                        self.finish_rect(bbox, Some(&pts))
                    } else {
                        None
                    }
                }
                CaptureMode::Window | CaptureMode::Fullscreen => None,
            },
            OverlayMessage::ConfirmFullscreen => self.finish_fullscreen(),
            OverlayMessage::Cancel => Some(OverlayOutcome::Cancelled),
        }
    }

    fn find_window_at(&self, pt: Point) -> Option<Rect> {
        self.x11_windows
            .iter()
            .rev()
            .find(|w| w.rect.contains(pt))
            .map(|w| {
                w.rect
                    .clamp(self.captured.width as f32, self.captured.height as f32)
            })
    }

    fn finish_fullscreen(&self) -> Option<OverlayOutcome> {
        Some(OverlayOutcome::Captured {
            rgba: self.captured.rgba.clone(),
            width: self.captured.width,
            height: self.captured.height,
        })
    }

    fn finish_rect(&self, rect: Rect, lasso: Option<&[Point]>) -> Option<OverlayOutcome> {
        let (rgba, width, height) = crop_captured_selection(&self.captured, rect, lasso).ok()?;
        Some(OverlayOutcome::Captured {
            rgba,
            width,
            height,
        })
    }

    /// Renders the overlay for the given output geometry (or full desktop if `None`).
    pub fn view(&self, output: Option<&OutputGeometry>) -> Element<'_, OverlayMessage> {
        let (ox, oy, ow, oh) = match output {
            Some(out) => (
                out.x as f32,
                out.y as f32,
                out.width as f32,
                out.height as f32,
            ),
            None => (
                0.0,
                0.0,
                self.captured.width as f32,
                self.captured.height as f32,
            ),
        };

        let canvas_overlay = iced::widget::canvas(OverlayCanvas {
            state: self,
            origin_x: ox,
            origin_y: oy,
            logical_w: ow,
            logical_h: oh,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        let toolbar = self.view_mode_bar();

        let top_bar = container(toolbar)
            .width(Length::Fill)
            .padding(12)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Top);

        if let Some(bg_handle) = &self.bg_handle {
            let bg = image(bg_handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Fill);
            stack![bg, canvas_overlay, top_bar]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            stack![canvas_overlay, top_bar]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    }

    fn view_mode_bar(&self) -> Element<'_, OverlayMessage> {
        let rect_btn = pill_icon_button(
            icons::ICON_RECT,
            "Retângulo",
            self.mode == CaptureMode::Rect,
            OverlayMessage::SelectMode(CaptureMode::Rect),
        );
        let free_btn = pill_icon_button(
            icons::ICON_FREEFORM,
            "Livre",
            self.mode == CaptureMode::Freeform,
            OverlayMessage::SelectMode(CaptureMode::Freeform),
        );
        let window_btn = pill_icon_button(
            icons::ICON_WINDOW,
            "Janela",
            self.mode == CaptureMode::Window,
            OverlayMessage::SelectMode(CaptureMode::Window),
        );
        let full_btn = pill_icon_button(
            icons::ICON_FULLSCREEN,
            "Fullscreen",
            self.mode == CaptureMode::Fullscreen,
            OverlayMessage::ConfirmFullscreen,
        );
        let record_btn = pill_icon_button(
            icons::ICON_RECORD,
            "Gravar tela",
            self.mode == CaptureMode::Record,
            OverlayMessage::SelectMode(CaptureMode::Record),
        );

        let next_delay = match self.delay_secs {
            0 => 3,
            3 => 5,
            5 => 10,
            _ => 0,
        };
        let delay_label = if self.delay_secs == 0 {
            "Delay: Off".to_string()
        } else {
            format!("Delay: {}s", self.delay_secs)
        };
        let delay_btn = button(
            row![
                icons::icon(icons::ICON_DELAY, 14.0, Color::from_rgb8(26, 26, 28)),
                text(delay_label).size(13),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(overlay_pill_btn(false))
        .on_press(OverlayMessage::SetDelay(next_delay));

        let cancel_btn = button(icons::icon(
            icons::ICON_CLOSE,
            14.0,
            Color::from_rgb8(26, 26, 28),
        ))
        .padding([6, 10])
        .style(overlay_pill_btn(false))
        .on_press(OverlayMessage::Cancel);

        let level1 =
            row![rect_btn, free_btn, window_btn, full_btn, record_btn, delay_btn, cancel_btn]
                .spacing(6)
                .align_y(Alignment::Center);

        let mut col = Column::new()
            .push(level1)
            .spacing(6)
            .align_x(Alignment::Center);

        if self.mode == CaptureMode::Record {
            let rec_rect_btn = pill_icon_button(
                icons::ICON_RECT,
                "Retângulo",
                self.record_target == RecordTarget::Rect,
                OverlayMessage::SelectRecordTarget(RecordTarget::Rect),
            );
            let rec_win_btn = pill_icon_button(
                icons::ICON_WINDOW,
                "Janela",
                self.record_target == RecordTarget::Window,
                OverlayMessage::SelectRecordTarget(RecordTarget::Window),
            );
            let rec_full_btn = pill_icon_button(
                icons::ICON_FULLSCREEN,
                "Fullscreen",
                self.record_target == RecordTarget::Fullscreen,
                OverlayMessage::SelectRecordTarget(RecordTarget::Fullscreen),
            );

            let audio_label = if self.record_system_audio {
                "Audio: On"
            } else {
                "Audio: Off"
            };
            let audio_btn = button(text(audio_label).size(13))
                .padding([6, 12])
                .style(overlay_pill_btn(self.record_system_audio))
                .on_press(OverlayMessage::ToggleSystemAudio);

            let mic_label = if self.record_mic {
                "Mic: On"
            } else {
                "Mic: Off"
            };
            let mic_btn = button(text(mic_label).size(13))
                .padding([6, 12])
                .style(overlay_pill_btn(self.record_mic))
                .on_press(OverlayMessage::ToggleMic);

            let start_rec_btn = button(
                row![
                    icons::icon(icons::ICON_RECORD, 14.0, Color::WHITE),
                    text("Iniciar gravação").size(13),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding([6, 14])
            .style(record_action_btn)
            .on_press(OverlayMessage::StartRecordNow);

            let level2 = row![
                rec_rect_btn,
                rec_win_btn,
                rec_full_btn,
                audio_btn,
                mic_btn,
                start_rec_btn,
            ]
            .spacing(6)
            .align_y(Alignment::Center);

            let hint = match self.record_target {
                RecordTarget::Fullscreen => {
                    "Padrão: Fullscreen — clique em Iniciar gravação ou escolha Retângulo / Janela"
                }
                RecordTarget::Rect => "Arraste um retângulo na tela para iniciar a gravação",
                RecordTarget::Window => "Clique na janela desejada para iniciar a gravação",
            };

            col = col
                .push(level2)
                .push(text(hint).size(12).color(Color::from_rgb8(82, 82, 92)));
        }

        Container::new(col)
            .padding([8, 12])
            .style(|_theme| container::Style {
                background: Some(iced::Background::Color(Color::from_rgba8(
                    243, 243, 246, 0.96,
                ))),
                border: iced::Border {
                    color: Color::from_rgba8(210, 210, 216, 0.9),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            })
            .into()
    }
}

fn record_action_btn(_theme: &iced::Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Color::from_rgb8(185, 28, 28),
        button::Status::Pressed => Color::from_rgb8(153, 27, 27),
        _ => Color::from_rgb8(220, 38, 38),
    };
    button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color: Color::WHITE,
        border: iced::Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn overlay_pill_btn(active: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style + Copy {
    move |_theme, status| {
        let bg = if active {
            Color::from_rgb8(0, 103, 192)
        } else {
            match status {
                button::Status::Hovered => Color::from_rgb8(228, 228, 234),
                button::Status::Pressed => Color::from_rgb8(214, 214, 222),
                _ => Color::TRANSPARENT,
            }
        };
        let fg = if active {
            Color::WHITE
        } else if matches!(status, button::Status::Disabled) {
            Color::from_rgb8(150, 150, 160)
        } else {
            Color::from_rgb8(26, 26, 28)
        };
        button::Style {
            background: Some(iced::Background::Color(bg)),
            text_color: fg,
            border: iced::Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

fn pill_icon_button<'a>(
    svg_bytes: &'static [u8],
    label: &'a str,
    active: bool,
    msg: OverlayMessage,
) -> button::Button<'a, OverlayMessage> {
    let icon_color = if active {
        Color::WHITE
    } else {
        Color::from_rgb8(26, 26, 28)
    };
    button(
        row![
            icons::icon(svg_bytes, 14.0, icon_color),
            text(label).size(13),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([6, 12])
    .style(overlay_pill_btn(active))
    .on_press(msg)
}

struct OverlayCanvas<'a> {
    state: &'a OverlayState,
    origin_x: f32,
    origin_y: f32,
    logical_w: f32,
    logical_h: f32,
}

impl<'a> OverlayCanvas<'a> {
    fn to_image_point(&self, bounds: Rectangle, pos: IcedPoint) -> Point {
        let scale_x = if bounds.width > 0.0 {
            self.logical_w / bounds.width
        } else {
            1.0
        };
        let scale_y = if bounds.height > 0.0 {
            self.logical_h / bounds.height
        } else {
            1.0
        };
        let img_scale_x = self.state.captured.width as f32 / self.logical_w.max(1.0);
        let img_scale_y = self.state.captured.height as f32 / self.logical_h.max(1.0);
        Point::new(
            (self.origin_x + pos.x * scale_x) * img_scale_x,
            (self.origin_y + pos.y * scale_y) * img_scale_y,
        )
    }

    fn to_canvas_point(&self, bounds: Rectangle, pt: Point) -> IcedPoint {
        let img_scale_x = self.state.captured.width as f32 / self.logical_w.max(1.0);
        let img_scale_y = self.state.captured.height as f32 / self.logical_h.max(1.0);
        let lx = pt.x / img_scale_x - self.origin_x;
        let ly = pt.y / img_scale_y - self.origin_y;
        let sx = bounds.width / self.logical_w.max(1.0);
        let sy = bounds.height / self.logical_h.max(1.0);
        IcedPoint::new(lx * sx, ly * sy)
    }
}

impl<'a> canvas::Program<OverlayMessage> for OverlayCanvas<'a> {
    type State = bool; // true while pointer button is held

    fn update(
        &self,
        is_dragging: &mut Self::State,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (event::Status, Option<OverlayMessage>) {
        let Some(pos) = cursor.position_in(bounds) else {
            return (event::Status::Ignored, None);
        };
        let img_pt = self.to_image_point(bounds, pos);

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                *is_dragging = true;
                (
                    event::Status::Captured,
                    Some(OverlayMessage::PointerDown(img_pt)),
                )
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hover_window = self.state.mode == CaptureMode::Window
                    || (self.state.mode == CaptureMode::Record
                        && self.state.record_target == RecordTarget::Window);
                if *is_dragging || hover_window {
                    (
                        event::Status::Captured,
                        Some(OverlayMessage::PointerMove(img_pt)),
                    )
                } else {
                    (event::Status::Ignored, None)
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if *is_dragging {
                    *is_dragging = false;
                    (
                        event::Status::Captured,
                        Some(OverlayMessage::PointerUp(img_pt)),
                    )
                } else {
                    (event::Status::Ignored, None)
                }
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
        let dim = Color::from_rgba(0.0, 0.0, 0.0, 0.38);

        let has_rect = (self.state.mode == CaptureMode::Rect
            || (self.state.mode == CaptureMode::Record
                && self.state.record_target == RecordTarget::Rect))
            && self.state.drag_start.is_some()
            && self.state.drag_current.is_some();

        if !has_rect {
            // Dimmed backdrop over the frozen desktop.
            frame.fill_rectangle(
                IcedPoint::ORIGIN,
                Size::new(bounds.width, bounds.height),
                dim,
            );
        }

        let draw_rect_selection = self.state.mode == CaptureMode::Rect
            || (self.state.mode == CaptureMode::Record
                && self.state.record_target == RecordTarget::Rect);

        if draw_rect_selection {
            if let (Some(start), Some(cur)) = (self.state.drag_start, self.state.drag_current) {
                let a = self.to_canvas_point(bounds, start);
                let b = self.to_canvas_point(bounds, cur);
                let x = a.x.min(b.x).clamp(0.0, bounds.width);
                let y = a.y.min(b.y).clamp(0.0, bounds.height);
                let x2 = a.x.max(b.x).clamp(0.0, bounds.width);
                let y2 = a.y.max(b.y).clamp(0.0, bounds.height);
                let w = (x2 - x).max(0.0);
                let h = (y2 - y).max(0.0);

                if y > 0.0 {
                    frame.fill_rectangle(IcedPoint::ORIGIN, Size::new(bounds.width, y), dim);
                }
                if y2 < bounds.height {
                    frame.fill_rectangle(
                        IcedPoint::new(0.0, y2),
                        Size::new(bounds.width, bounds.height - y2),
                        dim,
                    );
                }
                if x > 0.0 && h > 0.0 {
                    frame.fill_rectangle(IcedPoint::new(0.0, y), Size::new(x, h), dim);
                }
                if x2 < bounds.width && h > 0.0 {
                    frame.fill_rectangle(
                        IcedPoint::new(x2, y),
                        Size::new(bounds.width - x2, h),
                        dim,
                    );
                }

                let stroke_color = if self.state.mode == CaptureMode::Record {
                    Color::from_rgb8(239, 68, 68)
                } else {
                    Color::from_rgb8(96, 165, 250)
                };
                let sel = Path::rectangle(IcedPoint::new(x, y), Size::new(w, h));
                frame.stroke(
                    &sel,
                    Stroke::default().with_color(stroke_color).with_width(2.0),
                );
            }
        } else if self.state.mode == CaptureMode::Freeform {
            if self.state.lasso_points.len() >= 2 {
                let lasso_path = Path::new(|builder| {
                    let first = self.to_canvas_point(bounds, self.state.lasso_points[0]);
                    builder.move_to(first);
                    for pt in &self.state.lasso_points[1..] {
                        builder.line_to(self.to_canvas_point(bounds, *pt));
                    }
                });
                frame.stroke(
                    &lasso_path,
                    Stroke::default()
                        .with_color(Color::from_rgb8(96, 165, 250))
                        .with_width(2.0),
                );
            }
        } else if self.state.mode == CaptureMode::Window
            || (self.state.mode == CaptureMode::Record
                && self.state.record_target == RecordTarget::Window)
        {
            if let Some(win_rect) = self.state.hovered_window {
                let tl = self.to_canvas_point(bounds, Point::new(win_rect.x, win_rect.y));
                let br = self.to_canvas_point(
                    bounds,
                    Point::new(win_rect.x + win_rect.width, win_rect.y + win_rect.height),
                );
                let p = Path::rectangle(tl, Size::new(br.x - tl.x, br.y - tl.y));
                let (fill_color, stroke_color) = if self.state.mode == CaptureMode::Record {
                    (
                        Color::from_rgba8(239, 68, 68, 0.16),
                        Color::from_rgb8(239, 68, 68),
                    )
                } else {
                    (
                        Color::from_rgba8(96, 165, 250, 0.16),
                        Color::from_rgb8(96, 165, 250),
                    )
                };
                frame.fill(&p, fill_color);
                frame.stroke(
                    &p,
                    Stroke::default().with_color(stroke_color).with_width(2.5),
                );
            }
        }

        vec![frame.into_geometry()]
    }
}

/// Crops `captured` to `rect` and, if `lasso` is provided, masks pixels outside the polygon
/// to transparent (`alpha = 0`).
pub fn crop_captured_selection(
    captured: &CapturedImage,
    rect: Rect,
    lasso: Option<&[Point]>,
) -> Result<(Vec<u8>, u32, u32), String> {
    let clamped = rect.clamp(captured.width as f32, captured.height as f32);
    let x0 = (clamped.x.floor() as u32).min(captured.width);
    let y0 = (clamped.y.floor() as u32).min(captured.height);
    let x1 = ((clamped.x + clamped.width).ceil() as u32).min(captured.width);
    let y1 = ((clamped.y + clamped.height).ceil() as u32).min(captured.height);

    let w = x1.saturating_sub(x0);
    let h = y1.saturating_sub(y0);
    if w == 0 || h == 0 {
        return Err("empty selection".to_string());
    }

    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let src_stride = (captured.width as usize) * 4;
    let dst_stride = (w as usize) * 4;

    for row in 0..(h as usize) {
        let src_start = ((y0 as usize) + row) * src_stride + (x0 as usize) * 4;
        let dst_start = row * dst_stride;
        out[dst_start..dst_start + dst_stride]
            .copy_from_slice(&captured.rgba[src_start..src_start + dst_stride]);
    }

    if let Some(poly) = lasso {
        if poly.len() >= 3 {
            let mut pb = PathBuilder::new();
            pb.move_to(poly[0].x - x0 as f32, poly[0].y - y0 as f32);
            for pt in &poly[1..] {
                pb.line_to(pt.x - x0 as f32, pt.y - y0 as f32);
            }
            pb.close();
            if let (Some(path), Some(mut mask)) = (pb.finish(), Mask::new(w, h)) {
                mask.fill_path(&path, FillRule::EvenOdd, true, Transform::identity());
                for (idx, alpha) in mask.data().iter().enumerate() {
                    if *alpha == 0 {
                        let px = idx * 4;
                        out[px] = 0;
                        out[px + 1] = 0;
                        out[px + 2] = 0;
                        out[px + 3] = 0;
                    }
                }
            }
        }
    }

    Ok((out, w, h))
}

fn dist_sq(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

fn bounding_box(points: &[Point]) -> Rect {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for pt in points {
        min_x = min_x.min(pt.x);
        min_y = min_y.min(pt.y);
        max_x = max_x.max(pt.x);
        max_y = max_y.max(pt.y);
    }
    Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_rect_extracts_expected_region() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        // Mark pixel (2, 1) red.
        let idx = (4 + 2) * 4;
        rgba[idx] = 255;
        rgba[idx + 3] = 255;

        let captured = CapturedImage {
            rgba,
            width: 4,
            height: 4,
            outputs: vec![],
        };

        let (cropped, w, h) =
            crop_captured_selection(&captured, Rect::new(1.0, 1.0, 2.0, 2.0), None).unwrap();
        assert_eq!((w, h), (2, 2));
        // Pixel (2, 1) in original is (1, 0) in cropped.
        let out_idx = 4;
        assert_eq!(cropped[out_idx], 255);
        assert_eq!(cropped[out_idx + 3], 255);
    }

    #[test]
    fn record_level2_supports_default_fullscreen_rect_drag_and_wayland_window() {
        let captured = CapturedImage {
            rgba: vec![0u8; 200 * 100 * 4],
            width: 200,
            height: 100,
            outputs: vec![],
        };
        let mut st = OverlayState::new(captured, CaptureMode::Rect, 0, false, vec![]);

        // Level 1 -> Record opens Level 2 defaulting to Fullscreen (does not start immediately).
        assert!(st
            .update(OverlayMessage::SelectMode(CaptureMode::Record))
            .is_none());
        assert_eq!(st.mode, CaptureMode::Record);
        assert_eq!(st.record_target, RecordTarget::Fullscreen);

        // Stray click in default Fullscreen does not accidentally start recording.
        assert!(st
            .update(OverlayMessage::PointerDown(Point::new(10.0, 10.0)))
            .is_none());

        // "Iniciar gravação" starts Fullscreen immediately.
        let outcome = st.update(OverlayMessage::StartRecordNow);
        assert!(matches!(
            outcome,
            Some(OverlayOutcome::StartRecording {
                target: RecordingTarget::Fullscreen,
                ..
            })
        ));

        // Selecting Retângulo in Level 2 lets the user drag a rect and immediately start.
        assert!(st
            .update(OverlayMessage::SelectRecordTarget(RecordTarget::Rect))
            .is_none());
        st.update(OverlayMessage::PointerDown(Point::new(20.0, 20.0)));
        st.update(OverlayMessage::PointerMove(Point::new(100.0, 80.0)));
        let drag_outcome = st.update(OverlayMessage::PointerUp(Point::new(100.0, 80.0)));
        assert!(matches!(
            drag_outcome,
            Some(OverlayOutcome::StartRecording {
                target: RecordingTarget::Region {
                    full_width: 200,
                    full_height: 100,
                    ..
                },
                ..
            })
        ));

        // Selecting Janela in Level 2 on Wayland (!supports_window_mode) immediately triggers Window portal recording.
        let win_outcome = st.update(OverlayMessage::SelectRecordTarget(RecordTarget::Window));
        assert!(matches!(
            win_outcome,
            Some(OverlayOutcome::StartRecording {
                target: RecordingTarget::Window,
                ..
            })
        ));
    }
}
