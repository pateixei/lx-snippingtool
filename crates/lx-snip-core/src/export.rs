//! Destructive rasterization and metadata-free image export.

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use thiserror::Error;
use tiny_skia::{
    BlendMode, ColorU8, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect as SkiaRect, Stroke,
    Transform,
};

use crate::scene::{AnnotationOp, AnnotationScene, Color, Point, Rect, ShapeKind};

/// Default JPEG export quality (`1..=100`).
pub const DEFAULT_JPEG_QUALITY: u8 = 90;

/// Supported output image formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    /// Lossless PNG without any metadata (`eXIf`, `tEXt`, `zTXt`, `iTXt`) chunks.
    #[default]
    Png,
    /// Lossy JPEG with transparent pixels composited onto opaque white.
    Jpeg {
        /// Encoder quality in `1..=100` (clamped if outside range).
        quality: u8,
    },
}

/// Errors produced during scene rasterization or encoding.
#[derive(Debug, Error)]
pub enum ExportError {
    /// Base image dimensions were zero.
    #[error("invalid image dimensions: width and height must be positive")]
    InvalidDimensions,
    /// Base RGBA buffer length did not match `width * height * 4`.
    #[error("RGBA buffer size mismatch: expected {expected} bytes, got {actual}")]
    BufferSizeMismatch {
        /// Expected byte length (`width * height * 4`).
        expected: usize,
        /// Actual byte length provided.
        actual: usize,
    },
    /// Crop rectangle produced zero width or height after clamping.
    #[error("crop rectangle is empty after clamping to image bounds")]
    EmptyCrop,
    /// `tiny-skia` failed to allocate a pixmap for the target dimensions.
    #[error("failed to allocate rasterization pixmap")]
    PixmapAllocationFailed,
    /// Image encoder failed.
    #[error("failed to encode output image: {0}")]
    Encode(String),
}

/// Rasterizes `scene` onto `base_rgba` (`width` x `height` straight RGBA8) and returns
/// the cropped straight RGBA8 buffer along with its output `(width, height)`.
pub fn render_to_rgba(
    base_rgba: &[u8],
    width: u32,
    height: u32,
    scene: &AnnotationScene,
) -> Result<(Vec<u8>, u32, u32), ExportError> {
    if width == 0 || height == 0 {
        return Err(ExportError::InvalidDimensions);
    }

    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or(ExportError::InvalidDimensions)?;
    if base_rgba.len() != expected {
        return Err(ExportError::BufferSizeMismatch {
            expected,
            actual: base_rgba.len(),
        });
    }

    let (crop_x, crop_y, crop_w, crop_h) = resolve_crop(scene.crop_rect, width, height)?;

    let mut pixmap = Pixmap::new(crop_w, crop_h).ok_or(ExportError::PixmapAllocationFailed)?;
    copy_cropped_rgba_to_pixmap(
        base_rgba,
        width,
        crop_x,
        crop_y,
        crop_w,
        crop_h,
        &mut pixmap,
    );

    let transform = Transform::from_translate(-(crop_x as f32), -(crop_y as f32));
    for op in scene.ops() {
        draw_op(&mut pixmap, op, transform);
    }

    let mut out_rgba = Vec::with_capacity((crop_w as usize) * (crop_h as usize) * 4);
    for px in pixmap.pixels() {
        let straight = px.demultiply();
        out_rgba.push(straight.red());
        out_rgba.push(straight.green());
        out_rgba.push(straight.blue());
        out_rgba.push(straight.alpha());
    }

    Ok((out_rgba, crop_w, crop_h))
}

/// Rasterizes `scene` onto `base_rgba` and encodes the result in `format` without EXIF
/// or PNG text chunks.
pub fn render_and_export(
    base_rgba: &[u8],
    width: u32,
    height: u32,
    scene: &AnnotationScene,
    format: ExportFormat,
) -> Result<Vec<u8>, ExportError> {
    let (rgba, out_w, out_h) = render_to_rgba(base_rgba, width, height, scene)?;
    encode_rgba(&rgba, out_w, out_h, format)
}

/// Encodes straight RGBA8 pixels into PNG or JPEG without metadata.
pub fn encode_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    format: ExportFormat,
) -> Result<Vec<u8>, ExportError> {
    let mut out = Vec::new();
    match format {
        ExportFormat::Png => {
            let encoder = PngEncoder::new(&mut out);
            encoder
                .write_image(rgba, width, height, ExtendedColorType::Rgba8)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Jpeg { quality } => {
            let rgb = composite_rgba_onto_white(rgba);
            let encoder = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
            encoder
                .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
    }
    Ok(out)
}

fn resolve_crop(
    crop_rect: Option<Rect>,
    width: u32,
    height: u32,
) -> Result<(u32, u32, u32, u32), ExportError> {
    let Some(raw) = crop_rect else {
        return Ok((0, 0, width, height));
    };

    let clamped = raw.clamp(width as f32, height as f32);
    let x0 = (clamped.x.floor() as u32).min(width);
    let y0 = (clamped.y.floor() as u32).min(height);
    let x1 = ((clamped.x + clamped.width).ceil() as u32).min(width);
    let y1 = ((clamped.y + clamped.height).ceil() as u32).min(height);

    let cw = x1.saturating_sub(x0);
    let ch = y1.saturating_sub(y0);
    if cw == 0 || ch == 0 {
        return Err(ExportError::EmptyCrop);
    }
    Ok((x0, y0, cw, ch))
}

fn copy_cropped_rgba_to_pixmap(
    base_rgba: &[u8],
    base_width: u32,
    crop_x: u32,
    crop_y: u32,
    crop_w: u32,
    crop_h: u32,
    pixmap: &mut Pixmap,
) {
    let dst_pixels = pixmap.pixels_mut();
    let base_stride = (base_width as usize) * 4;
    let cw = crop_w as usize;

    for row in 0..(crop_h as usize) {
        let src_y = (crop_y as usize) + row;
        let src_row_start = src_y * base_stride + (crop_x as usize) * 4;
        let dst_row_start = row * cw;

        for col in 0..cw {
            let idx = src_row_start + col * 4;
            let premul = ColorU8::from_rgba(
                base_rgba[idx],
                base_rgba[idx + 1],
                base_rgba[idx + 2],
                base_rgba[idx + 3],
            )
            .premultiply();
            dst_pixels[dst_row_start + col] = premul;
        }
    }
}

fn composite_rgba_onto_white(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((rgba.len() / 4) * 3);
    for px in rgba.as_chunks::<4>().0 {
        let a = px[3] as u16;
        let inv_a = 255 - a;
        let r = ((px[0] as u16 * a + 255 * inv_a + 127) / 255) as u8;
        let g = ((px[1] as u16 * a + 255 * inv_a + 127) / 255) as u8;
        let b = ((px[2] as u16 * a + 255 * inv_a + 127) / 255) as u8;
        rgb.push(r);
        rgb.push(g);
        rgb.push(b);
    }
    rgb
}

fn draw_op(pixmap: &mut Pixmap, op: &AnnotationOp, transform: Transform) {
    match op {
        AnnotationOp::Pen {
            points,
            color,
            width,
        } => {
            draw_polyline(pixmap, points, *color, *width, transform);
        }
        AnnotationOp::Highlighter {
            points,
            color,
            width,
            opacity,
        } => {
            let alpha = ((color.a as f32) * opacity.clamp(0.0, 1.0)).round() as u8;
            let translucent = Color::rgba(color.r, color.g, color.b, alpha);
            draw_polyline(pixmap, points, translucent, *width, transform);
        }
        AnnotationOp::Shape {
            kind,
            start,
            end,
            color,
            width,
        } => {
            draw_shape(pixmap, *kind, *start, *end, *color, *width, transform);
        }
        AnnotationOp::RedactBox { rect } => {
            let r = rect.normalized();
            if let Some(skia_rect) = SkiaRect::from_xywh(r.x, r.y, r.width, r.height) {
                let mut paint = Paint::default();
                paint.set_color_rgba8(0, 0, 0, 255);
                paint.blend_mode = BlendMode::Source;
                paint.anti_alias = false;
                pixmap.fill_rect(skia_rect, &paint, transform, None);
            }
        }
    }
}

fn draw_polyline(
    pixmap: &mut Pixmap,
    points: &[Point],
    color: Color,
    width: f32,
    transform: Transform,
) {
    if points.is_empty() {
        return;
    }

    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;

    let stroke_width = width.max(0.5);
    if points.len() == 1 {
        let r = stroke_width * 0.5;
        if let Some(oval) = SkiaRect::from_xywh(points[0].x - r, points[0].y - r, r * 2.0, r * 2.0)
        {
            let mut pb = PathBuilder::new();
            pb.push_oval(oval);
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);
            }
        }
        return;
    }

    let mut pb = PathBuilder::new();
    pb.move_to(points[0].x, points[0].y);
    for pt in &points[1..] {
        pb.line_to(pt.x, pt.y);
    }

    if let Some(path) = pb.finish() {
        let stroke = Stroke {
            width: stroke_width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, transform, None);
    }
}

fn draw_shape(
    pixmap: &mut Pixmap,
    kind: ShapeKind,
    start: Point,
    end: Point,
    color: Color,
    width: f32,
    transform: Transform,
) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;

    let stroke_width = width.max(0.5);
    let stroke = Stroke {
        width: stroke_width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };

    let mut pb = PathBuilder::new();
    match kind {
        ShapeKind::Rectangle => {
            let r = Rect::new(start.x, start.y, end.x - start.x, end.y - start.y).normalized();
            if let Some(skia_rect) = SkiaRect::from_xywh(r.x, r.y, r.width, r.height) {
                pb.push_rect(skia_rect);
            }
        }
        ShapeKind::Ellipse => {
            let r = Rect::new(start.x, start.y, end.x - start.x, end.y - start.y).normalized();
            if let Some(skia_rect) = SkiaRect::from_xywh(r.x, r.y, r.width, r.height) {
                pb.push_oval(skia_rect);
            }
        }
        ShapeKind::Line => {
            pb.move_to(start.x, start.y);
            pb.line_to(end.x, end.y);
        }
        ShapeKind::Arrow => {
            pb.move_to(start.x, start.y);
            pb.line_to(end.x, end.y);

            let dx = end.x - start.x;
            let dy = end.y - start.y;
            let len = (dx * dx + dy * dy).sqrt();
            if len > 1e-3 {
                let angle = dy.atan2(dx);
                let head_len = (stroke_width * 3.5).max(12.0).min(len * 0.6);
                let wing_angle = std::f32::consts::FRAC_PI_6; // 30 degrees

                let left_x = end.x - head_len * (angle - wing_angle).cos();
                let left_y = end.y - head_len * (angle - wing_angle).sin();
                let right_x = end.x - head_len * (angle + wing_angle).cos();
                let right_y = end.y - head_len * (angle + wing_angle).sin();

                pb.move_to(left_x, left_y);
                pb.line_to(end.x, end.y);
                pb.line_to(right_x, right_y);
            }
        }
    }

    if let Some(path) = pb.finish() {
        pixmap.stroke_path(&path, &paint, &stroke, transform, None);
    }
}
