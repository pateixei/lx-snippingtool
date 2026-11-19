//! Windows 11 Fluent-inspired vector SVG icons for `lx-snippingtool`.

use iced::widget::svg;
use iced::{Color, Length};

/// App icon SVG (dashed snipping frame + scissors, inspired by Windows 11 Snipping Tool).
pub const APP_ICON_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none">
  <rect x="4" y="4" width="14" height="14" rx="2" stroke="#E0533C" stroke-width="2" stroke-dasharray="3 2"/>
  <circle cx="9" cy="18" r="2.2" stroke="#0078D4" stroke-width="1.8"/>
  <circle cx="17" cy="18" r="2.2" stroke="#0078D4" stroke-width="1.8"/>
  <path d="M10.4 16.3L16.5 8.5M15.6 16.3L9.5 8.5" stroke="#0078D4" stroke-width="1.8" stroke-linecap="round"/>
</svg>"##;

pub const ICON_RECT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><rect x="2.5" y="3.5" width="11" height="9" rx="1.2"/></svg>"#;
pub const ICON_FREEFORM: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><path d="M4.5 4C2.5 5 2.5 9.5 4.8 11.2C7 12.8 11.5 13 13 10.5C14.2 8.5 12 6.5 10.2 5.2C8.5 4 6.5 3 4.5 4Z"/></svg>"#;
pub const ICON_WINDOW: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><rect x="2" y="3" width="12" height="10" rx="1.2"/><path d="M2 6h12"/></svg>"#;
pub const ICON_FULLSCREEN: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><path d="M2.5 5.5V3h2.5M11 3h2.5v2.5M13.5 10.5V13H11M5 13H2.5v-2.5"/></svg>"#;
pub const ICON_DELAY: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><circle cx="8" cy="8.5" r="5.2"/><path d="M8 5.8V8.8L10 10M6.5 1.8h3"/></svg>"#;
pub const ICON_CLOSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M4 4l8 8M12 4l-8 8"/></svg>"#;
pub const ICON_MINIMIZE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><path d="M3.5 8.5h9"/></svg>"#;
pub const ICON_MAXIMIZE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><rect x="3.5" y="3.5" width="9" height="9" rx="1"/></svg>"#;

pub const ICON_PEN: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M11.5 2.5l2 2L5.5 12.5l-3 .7.7-3 8.3-7.7z"/></svg>"#;
pub const ICON_HIGHLIGHTER: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M10.5 2.5l3 3-6.5 6.5-3.5.5.5-3.5 6.5-6.5zM2.5 14h11"/></svg>"#;
pub const ICON_ERASER: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M7 13.5h6.5M3.2 9.5l5.3-5.3a1.5 1.5 0 012.1 0l2.2 2.2a1.5 1.5 0 010 2.1L8.5 12.8a1.5 1.5 0 01-2.1 0L3.2 9.6a1.5 1.5 0 010-2.1z"/></svg>"#;
pub const ICON_LINE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M3 13L13 3"/></svg>"#;
pub const ICON_ELLIPSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><circle cx="8" cy="8" r="5.2"/></svg>"#;
pub const ICON_ARROW: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 13L13 3M7.5 3H13v5.5"/></svg>"#;
pub const ICON_REDACT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor"><rect x="2.5" y="4" width="11" height="8" rx="1.2"/></svg>"#;
pub const ICON_CROP: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><path d="M4.5 2v9.5H14M2 4.5h9.5V14"/></svg>"#;
pub const ICON_UNDO: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M5.5 4L2.5 7l3 3M3 7h6.5a3.5 3.5 0 110 7H7.5"/></svg>"#;
pub const ICON_REDO: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M10.5 4l3 3-3 3M13 7H6.5a3.5 3.5 0 100 7H8.5"/></svg>"#;
pub const ICON_SAVE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"><path d="M3 2.5h8l2.5 2.5v8.5h-11v-11z"/><path d="M5.5 2.5v3.5h4.5V2.5M5 13.5v-4h6v4"/></svg>"#;
pub const ICON_COPY: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"><rect x="5.5" y="5.5" width="8" height="8" rx="1.2"/><path d="M10.5 5.5V3.5a1 1 0 00-1-1h-6a1 1 0 00-1 1v6a1 1 0 001 1h2"/></svg>"#;
pub const ICON_HISTORY: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><path d="M2.8 8a5.2 5.2 0 101.2-3.4M2.5 2.8v2.4h2.4M8 5.5V8l2 1.2"/></svg>"#;
pub const ICON_RECORD: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"><rect x="2" y="4" width="8.5" height="8" rx="1.5"/><path d="M10.5 6.8l3.5-2v6.4l-3.5-2"/></svg>"#;
pub const ICON_STOP: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor"><rect x="3.5" y="3.5" width="9" height="9" rx="1.5"/></svg>"#;
pub const ICON_PAUSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor"><rect x="3.5" y="3.5" width="3" height="9" rx="1"/><rect x="9.5" y="3.5" width="3" height="9" rx="1"/></svg>"#;
pub const ICON_PLAY: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor"><path d="M4.5 3.2l8.5 4.8-8.5 4.8z"/></svg>"#;

/// Renders a monochrome SVG icon tinted to `color`.
pub fn icon<'a>(bytes: &'static [u8], size: f32, color: Color) -> svg::Svg<'a> {
    svg::Svg::new(svg::Handle::from_memory(bytes))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_theme, _status| svg::Style { color: Some(color) })
}

/// Renders the full-color app icon.
pub fn app_icon<'a>(size: f32) -> svg::Svg<'a> {
    svg::Svg::new(svg::Handle::from_memory(APP_ICON_SVG))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
}
