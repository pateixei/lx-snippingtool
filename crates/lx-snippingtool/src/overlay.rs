//! Multi-monitor freeze-frame selection overlay with cached static background,
//! floating mode pill toolbar, and rectangular/freeform/window/fullscreen selection.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{button, container, image, row, stack, text, tooltip, Column, Container};
use iced::{event, Alignment, Color, Element, Length, Point as IcedPoint, Rectangle, Size};
use lx_snip_core::config::CaptureMode;
use lx_snip_core::platform::{CapturedImage, OutputGeometry};
use lx_snip_core::scene::{Point, Rect};
use tiny_skia::{FillRule, Mask, PathBuilder, Transform};

use crate::capture_x11::X11WindowBounds;
use crate::icons;

/// Messages emitted by the freeze-frame overlay.
#[derive(Debug, Clone)]
pub enum OverlayMessage {
    /// Switch active capture mode in the floating pill toolbar.
    SelectMode(CaptureMode),
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
    /// Immediately capture the full desktop.
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
    /// Region selected for Phase 2 screen recording.
    StartRecording {
        /// Selected rectangle in image-pixel coordinates.
        rect: Rect,
        /// Full captured desktop width in pixels.
        full_width: u32,
        /// Full captured desktop height in pixels.
        full_height: u32,
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
    /// Active selection mode.
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
            record_system_audio: true,
            record_mic: false,
        }
    }

    /// Updates overlay state and returns an [`OverlayOutcome`] when the capture finishes or cancels.
    pub fn update(&mut self, msg: OverlayMessage) -> Option<OverlayOutcome> {
        match msg {
            OverlayMessage::SelectMode(mode) => {
                if mode == CaptureMode::Window && !self.supports_window_mode {
                    return None;
                }
                self.mode = mode;
                self.drag_start = None;
                self.drag_current = None;
                self.lasso_points.clear();
                self.hovered_window = None;
                if mode == CaptureMode::Fullscreen {
                    return self.finish_fullscreen();
                }
                None
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
                    CaptureMode::Rect | CaptureMode::Record => {
                        self.drag_start = Some(pt);
                        self.drag_current = Some(pt);
                    }
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
                    CaptureMode::Rect | CaptureMode::Record => {
                        if self.drag_start.is_some() {
                            self.drag_current = Some(pt);
                        }
                    }
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
                CaptureMode::Rect | CaptureMode::Record => {
                    let start = self.drag_start.take()?;
                    self.drag_current = None;
                    let rect = Rect::new(start.x, start.y, pt.x - start.x, pt.y - start.y)
                        .normalized()
                        .clamp(self.captured.width as f32, self.captured.height as f32);
                    if rect.width >= 4.0 && rect.height >= 4.0 {
                        if self.mode == CaptureMode::Record {
                            Some(OverlayOutcome::StartRecording {
                                rect,
                                full_width: self.captured.width,
                                full_height: self.captured.height,
                                with_system_audio: self.record_system_audio,
                                with_mic: self.record_mic,
                            })
                        } else {
                            self.finish_rect(rect, None)
                        }
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
        let rect_btn = mode_button(
            icons::ICON_RECT,
            "Rect",
            self.mode == CaptureMode::Rect,
            CaptureMode::Rect,
        );
        let free_btn = mode_button(
            icons::ICON_FREEFORM,
            "Freeform",
            self.mode == CaptureMode::Freeform,
            CaptureMode::Freeform,
        );
        let record_btn = mode_button(
            icons::ICON_RECORD,
            "Record",
            self.mode == CaptureMode::Record,
            CaptureMode::Record,
        );

        let window_btn: Element<'_, OverlayMessage> = if self.supports_window_mode {
            mode_button(
                icons::ICON_WINDOW,
                "Window",
                self.mode == CaptureMode::Window,
                CaptureMode::Window,
            )
            .into()
        } else {
            let disabled = button(
                row![
                    icons::icon(icons::ICON_WINDOW, 14.0, Color::from_rgb8(150, 150, 160)),
                    text("Window")
                        .size(13)
                        .color(Color::from_rgb8(150, 150, 160)),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding([6, 12])
            .style(overlay_pill_btn(false));
            tooltip(
                disabled,
                container(
                    text("Window capture uses the portal picker before freezing on Wayland (--mode window)")
                        .size(12),
                )
                .padding(6),
                tooltip::Position::Bottom,
            )
            .into()
        };

        let full_btn = button(
            row![
                icons::icon(icons::ICON_FULLSCREEN, 14.0, Color::from_rgb8(26, 26, 28)),
                text("Fullscreen").size(13),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(overlay_pill_btn(false))
        .on_press(OverlayMessage::ConfirmFullscreen);

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

        let mut bar = row![rect_btn, free_btn, window_btn, full_btn, record_btn]
            .spacing(6)
            .align_y(Alignment::Center);
        if self.mode == CaptureMode::Record {
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
            bar = bar.push(audio_btn).push(mic_btn);
        }
        let bar = bar.push(delay_btn).push(cancel_btn);

        Container::new(Column::new().push(bar))
            .padding([6, 10])
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

fn mode_button<'a>(
    svg_bytes: &'static [u8],
    label: &'a str,
    active: bool,
    mode: CaptureMode,
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
    .on_press(OverlayMessage::SelectMode(mode))
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
                if *is_dragging || self.state.mode == CaptureMode::Window {
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

        let has_rect = matches!(self.state.mode, CaptureMode::Rect | CaptureMode::Record)
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

        match self.state.mode {
            CaptureMode::Rect | CaptureMode::Record => {
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

                    let sel = Path::rectangle(IcedPoint::new(x, y), Size::new(w, h));
                    frame.stroke(
                        &sel,
                        Stroke::default()
                            .with_color(Color::from_rgb8(96, 165, 250))
                            .with_width(2.0),
                    );
                }
            }
            CaptureMode::Freeform => {
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
            }
            CaptureMode::Window => {
                if let Some(win_rect) = self.state.hovered_window {
                    let tl = self.to_canvas_point(bounds, Point::new(win_rect.x, win_rect.y));
                    let br = self.to_canvas_point(
                        bounds,
                        Point::new(win_rect.x + win_rect.width, win_rect.y + win_rect.height),
                    );
                    let p = Path::rectangle(tl, Size::new(br.x - tl.x, br.y - tl.y));
                    frame.fill(&p, Color::from_rgba8(96, 165, 250, 0.16));
                    frame.stroke(
                        &p,
                        Stroke::default()
                            .with_color(Color::from_rgb8(96, 165, 250))
                            .with_width(2.5),
                    );
                }
            }
            CaptureMode::Fullscreen => {}
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
}
