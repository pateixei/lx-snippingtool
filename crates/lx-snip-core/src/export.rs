//! Módulo: `export`
//! Objetivos: Rasterizar anotações, aplicar redações destrutivas e exportar PNG/JPEG sem metadados EXIF/texto.
//! Principais itens: `ExportFormat`, `ExportError`, `render_to_rgba`, `render_and_export` e `encode_rgba`.
//! Última alteração: 2026-11-19

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use thiserror::Error;
use tiny_skia::{
    Color as SkiaColor, ColorU8, GradientStop, LineCap, LineJoin, Paint, PathBuilder, Pixmap,
    Point as SkiaPoint, RadialGradient, Rect as SkiaRect, SpreadMode, Stroke, StrokeDash,
    Transform,
};

use crate::scene::{
    catmull_rom_spline_points, spray_stamp_points, AnnotationOp, AnnotationScene, Color, LineStyle,
    Point, Rect, ShapeKind,
};

/// Qualidade padrão de exportação JPEG (`1..=100`).
pub const DEFAULT_JPEG_QUALITY: u8 = 90;

/// Formatos de saída suportados na exportação de imagens.
/// Armazena: variantes `Png` e `Jpeg { quality }` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    /// PNG sem perdas e livre de chunks de metadados (`eXIf`, `tEXt`, `zTXt`, `iTXt`).
    #[default]
    Png,
    /// JPEG com composição prévia de transparência sobre fundo branco sólido.
    Jpeg {
        /// Qualidade do encoder entre `1` e `100`.
        quality: u8,
    },
}

/// Enumera erros ocorridos durante a rasterização ou codificação de uma cena anotada.
/// Armazena: variantes de dimensão inválida, tamanho de buffer, recorte vazio e falha de encoder | Principais métodos: `Display`, `Error`.
#[derive(Debug, Error)]
pub enum ExportError {
    /// Dimensões da imagem base iguais a zero ou inválidas.
    #[error("invalid image dimensions: width and height must be positive")]
    InvalidDimensions,
    /// Comprimento do buffer RGBA difere de `width * height * 4`.
    #[error("RGBA buffer size mismatch: expected {expected} bytes, got {actual}")]
    BufferSizeMismatch {
        /// Tamanho esperado em bytes.
        expected: usize,
        /// Tamanho efetivo recebido.
        actual: usize,
    },
    /// Retângulo de recorte resultou em largura ou altura nula.
    #[error("crop rectangle is empty after clamping to image bounds")]
    EmptyCrop,
    /// Falha ao alocar o pixmap do `tiny-skia`.
    #[error("failed to allocate rasterization pixmap")]
    PixmapAllocationFailed,
    /// Erro retornado pelo codificador PNG/JPEG.
    #[error("failed to encode output image: {0}")]
    Encode(String),
}

/// Rasteriza todas as operações de `scene` sobre `base_rgba` aplicando recorte e redação destrutiva.
/// Input: `base_rgba`, `width`, `height` e `scene` | Output: `Result<(Vec<u8>, u32, u32), ExportError>` em RGBA8 reto.
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
    let all_redact_rects: Vec<IntRect> = scene
        .ops()
        .iter()
        .filter_map(|op| match op {
            AnnotationOp::RedactBox { rect } => {
                rect_to_crop_int_rect(*rect, transform, crop_w, crop_h)
            }
            _ => None,
        })
        .collect();
    for op in scene.ops() {
        draw_op(&mut pixmap, op, transform, &all_redact_rects);
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

/// Rasteriza `scene` sobre `base_rgba` e codifica o resultado em PNG ou JPEG sem metadados.
/// Input: `base_rgba`, `width`, `height`, `scene` e `format` | Output: `Result<Vec<u8>, ExportError>` com bytes codificados.
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

/// Codifica pixels RGBA8 retos para o formato PNG ou JPEG sem chunks de metadados.
/// Input: slice `rgba`, `width`, `height` e `format` | Output: `Result<Vec<u8>, ExportError>` codificado.
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

/// Converte o retângulo opcional de recorte para coordenadas inteiras limitadas às dimensões da imagem.
/// Input: `crop_rect`, `width` e `height` | Output: `Result<(u32, u32, u32, u32), ExportError>` `(x, y, w, h)`.
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

/// Copia a sub-região recortada de RGBA8 reto para um `Pixmap` pré-multiplicado do `tiny-skia`.
/// Input: `base_rgba`, `base_width`, região (`crop_x`, `crop_y`, `crop_w`, `crop_h`) e `pixmap` | Output: `pixmap` preenchido.
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

/// Compõe pixels RGBA sobre fundo branco opaco gerando buffer RGB8 para exportação JPEG.
/// Input: slice `rgba` | Output: `Vec<u8>` com 3 bytes por pixel (RGB8).
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

/// Desenha uma única operação (`Pen`, `Highlighter`, `Spray`, `CurvedLine`, `Shape` ou `RedactBox`) sobre o `Pixmap`.
/// Input: `pixmap` destino, operação `op`, matriz `transform` e lista `all_redact_rects` | Output: desenha diretamente no `pixmap`.
fn draw_op(
    pixmap: &mut Pixmap,
    op: &AnnotationOp,
    transform: Transform,
    all_redact_rects: &[IntRect],
) {
    match op {
        AnnotationOp::Pen {
            points,
            color,
            width,
            style,
        } => {
            draw_polyline(pixmap, points, *color, *width, *style, transform);
        }
        AnnotationOp::Highlighter {
            points,
            color,
            width,
            opacity,
        } => {
            let alpha = ((color.a as f32) * opacity.clamp(0.0, 1.0)).round() as u8;
            let translucent = Color::rgba(color.r, color.g, color.b, alpha);
            draw_polyline(
                pixmap,
                points,
                translucent,
                *width,
                LineStyle::Solid,
                transform,
            );
        }
        AnnotationOp::Spray {
            points,
            color,
            radius,
        } => {
            draw_spray(pixmap, points, *color, *radius, transform);
        }
        AnnotationOp::CurvedLine {
            points,
            color,
            width,
            style,
        } => {
            let spline = catmull_rom_spline_points(points);
            draw_polyline(pixmap, &spline, *color, *width, *style, transform);
        }
        AnnotationOp::Shape {
            kind,
            start,
            end,
            color,
            width,
            style,
        } => {
            draw_shape(
                pixmap, *kind, *start, *end, *color, *width, *style, transform,
            );
        }
        AnnotationOp::RedactBox { rect } => {
            draw_redact_box(pixmap, *rect, transform, all_redact_rects);
        }
    }
}

/// Constrói o padrão opcional de tracejado/pontilhado `StrokeDash` conforme `style` e espessura `stroke_width`.
/// Input: `style` (`LineStyle`) e `stroke_width` (`f32`) | Output: `Option<StrokeDash>`.
fn build_stroke_dash(style: LineStyle, stroke_width: f32) -> Option<StrokeDash> {
    let w = stroke_width.max(1.0);
    match style {
        LineStyle::Solid => None,
        LineStyle::Dotted => StrokeDash::new(vec![0.1, w * 2.2], 0.0),
        LineStyle::Dashed => StrokeDash::new(vec![w * 3.0, w * 2.0], 0.0),
    }
}

/// Renderiza um traço livre poligonal suavizado no `Pixmap` com estilo de linha configurável.
/// Input: `pixmap`, vértices `points`, `color`, `width`, `style` e `transform` | Output: desenha o traço no `pixmap`.
fn draw_polyline(
    pixmap: &mut Pixmap,
    points: &[Point],
    color: Color,
    width: f32,
    style: LineStyle,
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
            dash: build_stroke_dash(style, stroke_width),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, transform, None);
    }
}

/// Renderiza jato suave estilo aerógrafo contínuo (gradientes radiais mesclados sem granulação pontual) ao longo da trajetória.
/// Input: `pixmap`, centros `points`, `color`, `radius` e `transform` | Output: desenha o spray mesclado no `pixmap`.
fn draw_spray(
    pixmap: &mut Pixmap,
    points: &[Point],
    color: Color,
    radius: f32,
    transform: Transform,
) {
    if points.is_empty() {
        return;
    }

    let soft_r = (radius * 1.35).max(5.0);
    let spacing = (soft_r * 0.25).clamp(1.5, 6.0);
    let stamps = spray_stamp_points(points, spacing);
    let boost = if stamps.len() == 1 { 2.2_f32 } else { 1.0 };

    let base_r = f32::from(color.r) / 255.0;
    let base_g = f32::from(color.g) / 255.0;
    let base_b = f32::from(color.b) / 255.0;
    let base_a = f32::from(color.a) / 255.0;
    let stop_color = |factor: f32| -> SkiaColor {
        let a = (factor * base_a * boost).clamp(0.0, 1.0);
        SkiaColor::from_rgba(base_r, base_g, base_b, a).unwrap_or(SkiaColor::TRANSPARENT)
    };
    let stops = vec![
        GradientStop::new(0.00, stop_color(0.22)),
        GradientStop::new(0.30, stop_color(0.14)),
        GradientStop::new(0.62, stop_color(0.045)),
        GradientStop::new(0.85, stop_color(0.010)),
        GradientStop::new(1.00, SkiaColor::TRANSPARENT),
    ];

    for pt in stamps {
        let center = SkiaPoint::from_xy(pt.x, pt.y);
        let Some(shader) = RadialGradient::new(
            center,
            center,
            soft_r,
            stops.clone(),
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            continue;
        };
        let Some(rect) =
            SkiaRect::from_xywh(pt.x - soft_r, pt.y - soft_r, soft_r * 2.0, soft_r * 2.0)
        else {
            continue;
        };
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        };
        pixmap.fill_rect(rect, &paint, transform, None);
    }
}

/// Retângulo inteiro exclusivo `[x0..x1, y0..y1]` no espaço de pixels do recorte atual.
/// Armazena: limites inteiros `x0`, `y0`, `x1`, `y1` | Principais métodos: `contains`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IntRect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl IntRect {
    /// Verifica se a coordenada inteira `(x, y)` pertence ao interior do retângulo.
    /// Input: `&self` e coordenadas `(x, y)` | Output: `bool` indicando cobertura.
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

/// Converte um `Rect` em coordenadas da cena para o `IntRect` que cobre todos os pixels tocados após `transform` e recorte.
/// Input: `rect`, `transform`, largura `crop_w` e altura `crop_h` | Output: `Some(IntRect)` se houver interseção válida.
fn rect_to_crop_int_rect(
    rect: Rect,
    transform: Transform,
    crop_w: u32,
    crop_h: u32,
) -> Option<IntRect> {
    let r = rect.normalized();
    if !r.x.is_finite()
        || !r.y.is_finite()
        || !r.width.is_finite()
        || !r.height.is_finite()
        || r.width <= 0.0
        || r.height <= 0.0
    {
        return None;
    }

    let max_x = i32::try_from(crop_w).unwrap_or(i32::MAX);
    let max_y = i32::try_from(crop_h).unwrap_or(i32::MAX);
    let x0 = (r.x + transform.tx).floor().clamp(0.0, max_x as f32) as i32;
    let y0 = (r.y + transform.ty).floor().clamp(0.0, max_y as f32) as i32;
    let x1 = (r.x + r.width + transform.tx)
        .ceil()
        .clamp(0.0, max_x as f32) as i32;
    let y1 = (r.y + r.height + transform.ty)
        .ceil()
        .clamp(0.0, max_y as f32) as i32;
    if x0 >= x1 || y0 >= y1 {
        return None;
    }

    Some(IntRect { x0, y0, x1, y1 })
}

/// Calcula valor pseudoaleatório determinístico uniforme em `[0.0, 1.0)` a partir apenas de coordenadas e semente.
/// Input: coordenadas globais `(x, y)` e `seed` | Output: valor `f32` em `[0.0, 1.0)`.
fn frosted_coord_hash01(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1)
        ^ (y as u32).wrapping_mul(0x85EB_CA77)
        ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h as f32) / ((u32::MAX as f32) + 1.0)
}

/// Executa uma passada de desfoque horizontal 1D com replicação de borda em `O(w * h)`.
/// Input: `src` (`&[[f32; 3]]`), largura `w`, altura `h` e raio `radius` | Output: `Vec<[f32; 3]>` desfocado horizontalmente.
fn box_blur_horizontal(src: &[[f32; 3]], w: usize, h: usize, radius: usize) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0_f32; 3]; w * h];
    let win = (radius * 2 + 1) as f64;
    let mut prefix = vec![[0.0_f64; 3]; w + 1];
    for y in 0..h {
        let row_off = y * w;
        prefix[0] = [0.0; 3];
        for x in 0..w {
            let p = src[row_off + x];
            prefix[x + 1] = [
                prefix[x][0] + f64::from(p[0]),
                prefix[x][1] + f64::from(p[1]),
                prefix[x][2] + f64::from(p[2]),
            ];
        }
        let first = src[row_off];
        let last = src[row_off + w - 1];
        for x in 0..w {
            let left_pad = radius.saturating_sub(x) as f64;
            let right_pad = (x + radius).saturating_sub(w - 1) as f64;
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius).min(w - 1) + 1;
            let dst = &mut out[row_off + x];
            for c in 0..3 {
                let sum = prefix[x1][c] - prefix[x0][c]
                    + left_pad * f64::from(first[c])
                    + right_pad * f64::from(last[c]);
                dst[c] = (sum / win) as f32;
            }
        }
    }
    out
}

/// Executa uma passada de desfoque vertical 1D com replicação de borda em `O(w * h)`.
/// Input: `src` (`&[[f32; 3]]`), largura `w`, altura `h` e raio `radius` | Output: `Vec<[f32; 3]>` desfocado verticalmente.
fn box_blur_vertical(src: &[[f32; 3]], w: usize, h: usize, radius: usize) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0_f32; 3]; w * h];
    let win = (radius * 2 + 1) as f64;
    let mut prefix = vec![[0.0_f64; 3]; h + 1];
    for x in 0..w {
        prefix[0] = [0.0; 3];
        for y in 0..h {
            let p = src[y * w + x];
            prefix[y + 1] = [
                prefix[y][0] + f64::from(p[0]),
                prefix[y][1] + f64::from(p[1]),
                prefix[y][2] + f64::from(p[2]),
            ];
        }
        let first = src[x];
        let last = src[(h - 1) * w + x];
        for y in 0..h {
            let top_pad = radius.saturating_sub(y) as f64;
            let bot_pad = (y + radius).saturating_sub(h - 1) as f64;
            let y0 = y.saturating_sub(radius);
            let y1 = (y + radius).min(h - 1) + 1;
            let dst = &mut out[y * w + x];
            for c in 0..3 {
                let sum = prefix[y1][c] - prefix[y0][c]
                    + top_pad * f64::from(first[c])
                    + bot_pad * f64::from(last[c]);
                dst[c] = (sum / win) as f32;
            }
        }
    }
    out
}

/// Aplica duas passadas separáveis de box blur em `src` (`w x h`) para destruir nitidez de letras e detalhes finos.
/// Input: `src` (`&[[f32; 3]]`), dimensões `w`/`h` e raio `radius` | Output: `Vec<[f32; 3]>` fortemente desfocado.
fn blur_rgb_two_pass(src: &[[f32; 3]], w: usize, h: usize, radius: usize) -> Vec<[f32; 3]> {
    if w == 0 || h == 0 || radius == 0 {
        return src.to_vec();
    }
    let pass1_h = box_blur_horizontal(src, w, h, radius);
    let pass1 = box_blur_vertical(&pass1_h, w, h, radius);
    let pass2_h = box_blur_horizontal(&pass1, w, h, radius);
    box_blur_vertical(&pass2_h, w, h, radius)
}

/// Mistura um pixel de destino com a cor do véu externo respeitando alfa pré-multiplicado do `Pixmap`.
/// Input: pixel atual no `pixmap`, cor `[r, g, b]` do véu e opacidade `src_alpha` | Output: pixel resultante.
fn blend_external_feather_pixel(
    existing: tiny_skia::PremultipliedColorU8,
    veil: [f32; 3],
    src_alpha: f32,
) -> tiny_skia::PremultipliedColorU8 {
    let dst = existing.demultiply();
    let dst_a = f32::from(dst.alpha()) / 255.0;
    let out_a = (src_alpha + dst_a * (1.0 - src_alpha)).clamp(0.0, 1.0);
    if out_a <= f32::EPSILON {
        return tiny_skia::PremultipliedColorU8::TRANSPARENT;
    }
    let out_r = (veil[0] * src_alpha + f32::from(dst.red()) * dst_a * (1.0 - src_alpha)) / out_a;
    let out_g = (veil[1] * src_alpha + f32::from(dst.green()) * dst_a * (1.0 - src_alpha)) / out_a;
    let out_b = (veil[2] * src_alpha + f32::from(dst.blue()) * dst_a * (1.0 - src_alpha)) / out_a;
    ColorU8::from_rgba(
        out_r.round().clamp(0.0, 255.0) as u8,
        out_g.round().clamp(0.0, 255.0) as u8,
        out_b.round().clamp(0.0, 255.0) as u8,
        (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
    )
    .premultiply()
}

/// Renderiza efeito de vidro fosco fumê (desfoque forte + camada escura 50% + dispersão estocástica em toda a área e perímetro).
/// Input: `pixmap`, `rect`, `transform` e lista `all_redact_rects` | Output: aplica o vidro fosco fumê no `pixmap`.
fn draw_redact_box(
    pixmap: &mut Pixmap,
    rect: Rect,
    transform: Transform,
    all_redact_rects: &[IntRect],
) {
    const DARK_VEIL: [f32; 3] = [12.0, 14.0, 18.0];
    const VEIL_ALPHA: f32 = 0.50;

    let w = pixmap.width();
    let h = pixmap.height();
    let Some(box_rect) = rect_to_crop_int_rect(rect, transform, w, h) else {
        return;
    };

    let bw = (box_rect.x1 - box_rect.x0).max(1) as usize;
    let bh = (box_rect.y1 - box_rect.y0).max(1) as usize;
    let stride = w as usize;

    // 1. Captura os pixels retos internos e aplica desfoque forte + camada escura a 50%.
    let mut orig_rgb = Vec::with_capacity(bw * bh);
    {
        let pixels = pixmap.pixels();
        for py in box_rect.y0..box_rect.y1 {
            let row_off = (py as usize) * stride;
            for px in box_rect.x0..box_rect.x1 {
                let s = pixels[row_off + (px as usize)].demultiply();
                orig_rgb.push([
                    f32::from(s.red()),
                    f32::from(s.green()),
                    f32::from(s.blue()),
                ]);
            }
        }
    }

    let blur_r = ((bw.min(bh) as f32) * 0.35).round().clamp(2.0, 12.0) as usize;
    let blurred = blur_rgb_two_pass(&orig_rgb, bw, bh, blur_r);
    let mut smoked = Vec::with_capacity(blurred.len());
    for px in blurred {
        smoked.push([
            px[0] * (1.0 - VEIL_ALPHA) + DARK_VEIL[0] * VEIL_ALPHA,
            px[1] * (1.0 - VEIL_ALPHA) + DARK_VEIL[1] * VEIL_ALPHA,
            px[2] * (1.0 - VEIL_ALPHA) + DARK_VEIL[2] * VEIL_ALPHA,
        ]);
    }

    let scatter_r = ((bw.min(bh) as f32) * 0.40).clamp(2.5, 16.0);
    let crop_off_x = (-transform.tx).round() as i32;
    let crop_off_y = (-transform.ty).round() as i32;

    // 2. Dispersão estocástica (frosted glass) em toda a superfície interna com alpha 100% opaco.
    {
        let pixels = pixmap.pixels_mut();
        for ly in 0..bh {
            let py = box_rect.y0 + (ly as i32);
            let gy = py + crop_off_y;
            let row_off = (py as usize) * stride;
            for lx in 0..bw {
                let px = box_rect.x0 + (lx as i32);
                let gx = px + crop_off_x;

                let ang = frosted_coord_hash01(gx, gy, 0x9E37_79B9) * std::f32::consts::TAU;
                let rad = frosted_coord_hash01(gx, gy, 0x85EB_CA6B).sqrt() * scatter_r;
                let sx = ((lx as f32) + ang.cos() * rad)
                    .round()
                    .clamp(0.0, (bw - 1) as f32) as usize;
                let sy = ((ly as f32) + ang.sin() * rad)
                    .round()
                    .clamp(0.0, (bh - 1) as f32) as usize;

                let base = smoked[sy * bw + sx];
                let grain = (frosted_coord_hash01(gx, gy, 0xC2B2_AE3D) * 2.0 - 1.0) * 9.0;
                let out_r = (base[0] + grain).round().clamp(0.0, 255.0) as u8;
                let out_g = (base[1] + grain).round().clamp(0.0, 255.0) as u8;
                let out_b = (base[2] + grain).round().clamp(0.0, 255.0) as u8;
                pixels[row_off + (px as usize)] =
                    ColorU8::from_rgba(out_r, out_g, out_b, 255).premultiply();
            }
        }
    }

    // 3. Dispersão estocástica nas bordas externas fora de outras tarjas.
    let pad = scatter_r.ceil() as i32;
    let fx0 = (box_rect.x0 - pad).max(0);
    let fy0 = (box_rect.y0 - pad).max(0);
    let fx1 = (box_rect.x1 + pad).min(w as i32);
    let fy1 = (box_rect.y1 + pad).min(h as i32);

    let pixels = pixmap.pixels_mut();
    for py in fy0..fy1 {
        let cy = (py as f32) + 0.5;
        let dy = if cy < (box_rect.y0 as f32) {
            (box_rect.y0 as f32) - cy
        } else if cy > (box_rect.y1 as f32) {
            cy - (box_rect.y1 as f32)
        } else {
            0.0
        };
        let gy = py + crop_off_y;
        let base_sy = (py - box_rect.y0).clamp(0, (bh as i32) - 1) as usize;
        for px in fx0..fx1 {
            if all_redact_rects.iter().any(|r| r.contains(px, py)) {
                continue;
            }
            let cx = (px as f32) + 0.5;
            let dx = if cx < (box_rect.x0 as f32) {
                (box_rect.x0 as f32) - cx
            } else if cx > (box_rect.x1 as f32) {
                cx - (box_rect.x1 as f32)
            } else {
                0.0
            };
            let dist = (dx * dx + dy * dy).sqrt();
            let t = (1.0 - dist / scatter_r).clamp(0.0, 1.0);
            if t <= 0.0 {
                continue;
            }
            let prob = t.powf(1.55);
            let gx = px + crop_off_x;
            let h_val = frosted_coord_hash01(gx, gy, 0x27D4_EB2D);
            if h_val < prob {
                let jitter_x =
                    ((frosted_coord_hash01(gx, gy, 0x9E37_79B9) * 2.0 - 1.0) * 6.0).round() as i32;
                let jitter_y =
                    ((frosted_coord_hash01(gx, gy, 0x85EB_CA6B) * 2.0 - 1.0) * 6.0).round() as i32;
                let sx = (px - box_rect.x0 + jitter_x).clamp(0, (bw as i32) - 1) as usize;
                let sy = ((base_sy as i32) + jitter_y).clamp(0, (bh as i32) - 1) as usize;
                let edge_col = smoked[sy * bw + sx];
                let grain = (frosted_coord_hash01(gx, gy, 0xC2B2_AE3D) * 2.0 - 1.0) * 6.0;
                let frosted_col = [
                    (edge_col[0] + grain).clamp(0.0, 255.0),
                    (edge_col[1] + grain).clamp(0.0, 255.0),
                    (edge_col[2] + grain).clamp(0.0, 255.0),
                ];
                let alpha = (0.72 + 0.28 * ((prob - h_val) / prob).sqrt()).clamp(0.0, 1.0);
                let idx = (py as usize) * stride + (px as usize);
                pixels[idx] = blend_external_feather_pixel(pixels[idx], frosted_col, alpha);
            }
        }
    }
}

/// Adiciona as duas hastes de uma ponta de seta ao `PathBuilder` apontando para `(tip_x, tip_y)` no ângulo `angle`.
/// Input: `pb`, `tip_x`, `tip_y`, `angle` e `head_len` | Output: acrescenta segmentos ao `pb`.
fn push_arrow_head(pb: &mut PathBuilder, tip_x: f32, tip_y: f32, angle: f32, head_len: f32) {
    let wing_angle = std::f32::consts::FRAC_PI_6;
    let left_x = tip_x - head_len * (angle - wing_angle).cos();
    let left_y = tip_y - head_len * (angle - wing_angle).sin();
    let right_x = tip_x - head_len * (angle + wing_angle).cos();
    let right_y = tip_y - head_len * (angle + wing_angle).sin();

    pb.move_to(left_x, left_y);
    pb.line_to(tip_x, tip_y);
    pb.line_to(right_x, right_y);
}

/// Renderiza uma forma geométrica (retângulo, elipse, linha, seta ou seta dupla) no `Pixmap`.
/// Input: `pixmap`, `kind`, pontos `start`/`end`, `color`, `width`, `style` e `transform` | Output: desenha a forma no `pixmap`.
#[allow(clippy::too_many_arguments)]
fn draw_shape(
    pixmap: &mut Pixmap,
    kind: ShapeKind,
    start: Point,
    end: Point,
    color: Color,
    width: f32,
    style: LineStyle,
    transform: Transform,
) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;

    let stroke_width = width.max(0.5);
    let body_stroke = Stroke {
        width: stroke_width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        dash: build_stroke_dash(style, stroke_width),
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
        ShapeKind::Line | ShapeKind::Arrow | ShapeKind::DoubleArrow => {
            pb.move_to(start.x, start.y);
            pb.line_to(end.x, end.y);
        }
    }

    if let Some(path) = pb.finish() {
        pixmap.stroke_path(&path, &paint, &body_stroke, transform, None);
    }

    if matches!(kind, ShapeKind::Arrow | ShapeKind::DoubleArrow) {
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let len = (dx * dx + dy * dy).sqrt();
        if len > 1e-3 {
            let angle = dy.atan2(dx);
            let head_len = (stroke_width * 3.5).max(12.0).min(len * 0.48);
            let mut head_pb = PathBuilder::new();
            push_arrow_head(&mut head_pb, end.x, end.y, angle, head_len);
            if kind == ShapeKind::DoubleArrow {
                push_arrow_head(
                    &mut head_pb,
                    start.x,
                    start.y,
                    angle + std::f32::consts::PI,
                    head_len,
                );
            }
            if let Some(head_path) = head_pb.finish() {
                let solid_stroke = Stroke {
                    width: stroke_width,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&head_path, &paint, &solid_stroke, transform, None);
            }
        }
    }
}
