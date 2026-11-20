//! Módulo: `scene_export_tests`
//! Objetivos: Testar a cena retida (`AnnotationScene`), estilos de linha, spray, setas duplas, borracha, recorte e exportação PNG/JPEG sem metadados.
//! Principais itens: testes de unidade e integração de `scene` e `export`.
//! Última alteração: 2026-11-19

use image::GenericImageView;
use lx_snip_core::export::{render_and_export, render_to_rgba, ExportError, ExportFormat};
use lx_snip_core::scene::{
    AnnotationOp, AnnotationScene, Color, LineStyle, Point, Rect, ShapeKind,
};

fn solid_rgba(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let mut buf = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for _ in 0..(width * height) {
        buf.extend_from_slice(&color);
    }
    buf
}

fn png_chunk_types(png_bytes: &[u8]) -> Vec<[u8; 4]> {
    assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let mut chunks = Vec::new();
    let mut offset = 8;
    while offset + 12 <= png_bytes.len() {
        let len = u32::from_be_bytes(png_bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = png_bytes[offset + 4..offset + 8].try_into().unwrap();
        chunks.push(kind);
        offset += 12 + len;
    }
    chunks
}

#[test]
fn scene_undo_redo_across_ops_erase_and_crop() {
    let mut scene = AnnotationScene::new(100, 100);
    assert!(!scene.can_undo());
    assert!(!scene.can_redo());

    let pen = AnnotationOp::Pen {
        points: vec![Point::new(10.0, 10.0), Point::new(50.0, 10.0)],
        color: Color::RED,
        width: 4.0,
        style: LineStyle::Solid,
    };
    scene.push_op(pen.clone());
    scene.set_crop(Some(Rect::new(5.0, 5.0, 80.0, 80.0)));
    assert!(scene.can_undo());
    assert_eq!(scene.ops().len(), 1);
    assert!(scene.crop_rect.is_some());

    // Undo crop
    assert!(scene.undo());
    assert!(scene.crop_rect.is_none());
    assert!(scene.can_redo());

    // Redo crop
    assert!(scene.redo());
    assert_eq!(scene.crop_rect, Some(Rect::new(5.0, 5.0, 80.0, 80.0)));

    // Erase pen and undo/redo erase
    assert!(scene.erase_at(Point::new(30.0, 11.0), 3.0));
    assert!(scene.ops().is_empty());
    assert!(scene.undo());
    assert_eq!(scene.ops(), &[pen]);
    assert!(scene.redo());
    assert!(scene.ops().is_empty());
}

#[test]
fn stroke_eraser_hit_tests_all_variants_and_topmost_order() {
    let mut scene = AnnotationScene::new(200, 200);

    let line = AnnotationOp::Shape {
        kind: ShapeKind::Line,
        start: Point::new(10.0, 10.0),
        end: Point::new(100.0, 10.0),
        color: Color::BLACK,
        width: 2.0,
        style: LineStyle::Solid,
    };
    let rect_shape = AnnotationOp::Shape {
        kind: ShapeKind::Rectangle,
        start: Point::new(20.0, 20.0),
        end: Point::new(80.0, 80.0),
        color: Color::RED,
        width: 2.0,
        style: LineStyle::Dotted,
    };
    let ellipse = AnnotationOp::Shape {
        kind: ShapeKind::Ellipse,
        start: Point::new(100.0, 100.0),
        end: Point::new(160.0, 140.0),
        color: Color::YELLOW,
        width: 2.0,
        style: LineStyle::Dashed,
    };
    let redact = AnnotationOp::RedactBox {
        rect: Rect::new(40.0, 40.0, 30.0, 30.0),
    };
    let highlighter = AnnotationOp::Highlighter {
        points: vec![Point::new(50.0, 55.0)],
        color: Color::YELLOW,
        width: 10.0,
        opacity: 0.5,
    };

    scene.push_op(line);
    scene.push_op(rect_shape);
    scene.push_op(ellipse);
    scene.push_op(redact);
    scene.push_op(highlighter);

    // Miss completely
    assert!(!scene.erase_at(Point::new(190.0, 190.0), 2.0));
    assert_eq!(scene.ops().len(), 5);

    // Overlapping point (50, 55) hits topmost (highlighter) first, then redact box
    assert!(scene.erase_at(Point::new(50.0, 55.0), 1.0));
    assert_eq!(scene.ops().len(), 4);
    assert!(scene.erase_at(Point::new(50.0, 55.0), 1.0));
    assert_eq!(scene.ops().len(), 3);

    // Ellipse outline hit (topmost point at cx=130, y=100)
    assert!(scene.erase_at(Point::new(130.0, 100.0), 2.0));
    assert_eq!(scene.ops().len(), 2);

    // Rectangle edge hit (right edge x=80, y=50), interior miss at (50, 50)
    assert!(!scene.erase_at(Point::new(50.0, 50.0), 2.0));
    assert!(scene.erase_at(Point::new(80.0, 50.0), 2.0));
    assert_eq!(scene.ops().len(), 1);

    // Line hit
    assert!(scene.erase_at(Point::new(55.0, 11.0), 2.0));
    assert!(scene.ops().is_empty());
}

#[test]
fn crop_and_destructive_redact_overwrite_pixels_and_strip_png_metadata() {
    // 20x20 image filled with bright green [10, 220, 30, 255]
    let base = solid_rgba(20, 20, [10, 220, 30, 255]);
    let mut scene = AnnotationScene::new(20, 20);
    scene.set_crop(Some(Rect::new(4.0, 4.0, 12.0, 12.0)));
    scene.push_op(AnnotationOp::RedactBox {
        rect: Rect::new(6.0, 6.0, 4.0, 4.0),
    });

    let png_bytes = render_and_export(&base, 20, 20, &scene, ExportFormat::Png).unwrap();

    // Verify zero metadata chunks (no eXIf, tEXt, zTXt, iTXt)
    let chunks = png_chunk_types(&png_bytes);
    for forbidden in [b"eXIf", b"tEXt", b"zTXt", b"iTXt"] {
        assert!(
            !chunks.contains(forbidden),
            "unexpected metadata chunk {:?}",
            std::str::from_utf8(forbidden)
        );
    }

    let decoded = image::load_from_memory(&png_bytes).unwrap();
    assert_eq!(decoded.dimensions(), (12, 12));
    let rgba = decoded.to_rgba8();

    // Redacted pixel at base (7, 7) -> cropped (3, 3) is darkened ~50% over bright green [10, 220, 30]
    let redacted_px = rgba.get_pixel(3, 3).0;
    assert_eq!(redacted_px[3], 255);
    assert!(
        (80..=150).contains(&redacted_px[1]),
        "expected ~50% smoked frosted green inside redact, got {redacted_px:?}"
    );
    // Unredacted corner pixel far from scatter band: base (15, 15) -> cropped (11, 11) must remain original green
    assert_eq!(rgba.get_pixel(11, 11).0, [10, 220, 30, 255]);
    // Exterior scatter ring around crop [2..6, 2..6] must contain stochastic dark frosted speckles AND untouched green pixels
    let mut ring_has_speckle = false;
    let mut ring_has_green = false;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if (2..6).contains(&x) && (2..6).contains(&y) {
                continue;
            }
            let p = rgba.get_pixel(x, y).0;
            if p[1] < 160 {
                ring_has_speckle = true;
            } else if p == [10, 220, 30, 255] {
                ring_has_green = true;
            }
        }
    }
    assert!(
        ring_has_speckle && ring_has_green,
        "expected stochastic frosted scatter outside box"
    );
}

#[test]
fn jpeg_export_composites_transparency_onto_white() {
    // 8x8 fully transparent image [0, 0, 255, 0]
    let base = solid_rgba(8, 8, [0, 0, 255, 0]);
    let scene = AnnotationScene::new(8, 8);

    let jpeg_bytes =
        render_and_export(&base, 8, 8, &scene, ExportFormat::Jpeg { quality: 95 }).unwrap();

    // Verify no APP1 EXIF marker (0xFF 0xE1)
    assert!(!jpeg_bytes.windows(2).any(|w| w == [0xFF, 0xE1]));

    let decoded = image::load_from_memory(&jpeg_bytes).unwrap().to_rgb8();
    let px = decoded.get_pixel(4, 4).0;
    // White background after JPEG quantization should be >= 250 in all channels
    assert!(px[0] >= 250 && px[1] >= 250 && px[2] >= 250, "got {px:?}");
}

#[test]
fn pen_highlighter_and_arrow_render_visible_pixels() {
    let base = solid_rgba(32, 32, [255, 255, 255, 255]);
    let mut scene = AnnotationScene::new(32, 32);
    scene.push_op(AnnotationOp::Pen {
        points: vec![Point::new(2.0, 4.0), Point::new(28.0, 4.0)],
        color: Color::RED,
        width: 3.0,
        style: LineStyle::Solid,
    });
    scene.push_op(AnnotationOp::Highlighter {
        points: vec![Point::new(2.0, 14.0), Point::new(28.0, 14.0)],
        color: Color::YELLOW,
        width: 4.0,
        opacity: 0.5,
    });
    scene.push_op(AnnotationOp::Shape {
        kind: ShapeKind::Arrow,
        start: Point::new(4.0, 24.0),
        end: Point::new(28.0, 24.0),
        color: Color::BLACK,
        width: 2.0,
        style: LineStyle::Solid,
    });

    let (out, w, h) = render_to_rgba(&base, 32, 32, &scene).unwrap();
    assert_eq!((w, h), (32, 32));

    let pixel_at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    // Pen pixel should be red-tinted
    let pen_px = pixel_at(15, 4);
    assert!(pen_px[0] > 180 && pen_px[1] < 80, "pen_px={pen_px:?}");

    // Highlighter pixel should be yellow-tinted (high R & G, lower B)
    let hi_px = pixel_at(15, 14);
    assert!(
        hi_px[0] > 200 && hi_px[1] > 200 && hi_px[2] < 200,
        "hi_px={hi_px:?}"
    );

    // Arrow shaft pixel should be dark
    let arrow_px = pixel_at(15, 24);
    assert!(
        arrow_px[0] < 50 && arrow_px[1] < 50 && arrow_px[2] < 50,
        "arrow_px={arrow_px:?}"
    );
}

#[test]
fn export_validates_dimensions_buffer_and_crop() {
    let scene = AnnotationScene::new(4, 4);
    assert!(matches!(
        render_to_rgba(&[], 0, 4, &scene),
        Err(ExportError::InvalidDimensions)
    ));
    assert!(matches!(
        render_to_rgba(&[0u8; 8], 4, 4, &scene),
        Err(ExportError::BufferSizeMismatch {
            expected: 64,
            actual: 8
        })
    ));

    let mut bad_crop_scene = AnnotationScene::new(4, 4);
    bad_crop_scene.set_crop(Some(Rect::new(10.0, 10.0, 5.0, 5.0)));
    let bad_crop_scene_res = render_to_rgba(&[0u8; 64], 4, 4, &bad_crop_scene);
    assert!(matches!(bad_crop_scene_res, Err(ExportError::EmptyCrop)));
}

#[test]
fn spray_double_arrow_dotted_dashed_and_feathered_redact_render() {
    let base = solid_rgba(40, 40, [255, 255, 255, 255]);
    let mut scene = AnnotationScene::new(40, 40);

    scene.push_op(AnnotationOp::Spray {
        points: vec![Point::new(10.0, 10.0), Point::new(14.0, 10.0)],
        color: Color::RED,
        radius: 6.0,
    });
    scene.push_op(AnnotationOp::CurvedLine {
        points: vec![
            Point::new(4.0, 15.0),
            Point::new(20.0, 6.0),
            Point::new(36.0, 15.0),
        ],
        color: Color::RED,
        width: 2.0,
        style: LineStyle::Dotted,
    });
    scene.push_op(AnnotationOp::Shape {
        kind: ShapeKind::DoubleArrow,
        start: Point::new(5.0, 20.0),
        end: Point::new(35.0, 20.0),
        color: Color::BLACK,
        width: 2.0,
        style: LineStyle::Dashed,
    });
    scene.push_op(AnnotationOp::RedactBox {
        rect: Rect::new(12.0, 28.0, 10.0, 8.0),
    });

    assert!(scene.erase_at(Point::new(12.0, 10.0), 3.0));
    assert!(scene.undo());

    let (out, w, h) = render_to_rgba(&base, 40, 40, &scene).unwrap();
    assert_eq!((w, h), (40, 40));

    let pixel_at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    // Inside RedactBox over pure white [255, 255, 255], smoked veil (~50% dark [12, 14, 18]) yields frosted mid-gray in [105..=165]
    let center_px = pixel_at(17, 32);
    assert_eq!(center_px[3], 255);
    assert!(
        (105..=165).contains(&center_px[0])
            && (105..=165).contains(&center_px[1])
            && (105..=165).contains(&center_px[2]),
        "expected ~50% smoked frosted mid-gray over white, got {center_px:?}"
    );
    let border_inside_px = pixel_at(12, 32);
    assert_eq!(border_inside_px[3], 255);
    assert!(
        (105..=165).contains(&border_inside_px[0]),
        "expected smoked frosted border pixel, got {border_inside_px:?}"
    );
    // Exterior scatter ring [9..25, 25..39] outside [12..22, 28..36] shows frosted particle speckles + untouched white background
    let mut has_dark_speckle = false;
    let mut has_untouched_white = false;
    for y in 25..39u32 {
        for x in 9..25u32 {
            if (12..22).contains(&x) && (28..36).contains(&y) {
                continue;
            }
            let p = pixel_at(x, y);
            if p[0] < 200 {
                has_dark_speckle = true;
            } else if p == [255, 255, 255, 255] {
                has_untouched_white = true;
            }
        }
    }
    assert!(
        has_dark_speckle && has_untouched_white,
        "expected Paint.NET-style stochastic frosted scatter outside box"
    );
}

#[test]
fn smoked_frosted_glass_destroys_fine_text_contrast_while_preserving_broad_shapes() {
    let width = 96u32;
    let height = 48u32;
    let mut base = solid_rgba(width, height, [255, 255, 255, 255]);

    // Left half [0..48]: bright white paper with high-contrast 2px alternating black text-like stripes
    for y in 0..height {
        for x in 0..48u32 {
            let idx = ((y * width + x) * 4) as usize;
            if (x / 2).is_multiple_of(2) && (10..38).contains(&y) {
                base[idx..idx + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    // Right half [48..96]: dark terminal panel [20, 24, 40]
    for y in 0..height {
        for x in 48..width {
            let idx = ((y * width + x) * 4) as usize;
            base[idx..idx + 4].copy_from_slice(&[20, 24, 40, 255]);
        }
    }

    let mut scene = AnnotationScene::new(width, height);
    scene.set_crop(Some(Rect::new(4.0, 2.0, 88.0, 44.0)));
    scene.push_op(AnnotationOp::RedactBox {
        rect: Rect::new(10.3, 6.4, 74.5, 34.2),
    });
    scene.push_op(AnnotationOp::RedactBox {
        rect: Rect::new(30.2, 8.1, 0.4, 20.0),
    });

    let (rgba, cw, ch) = render_to_rgba(&base, width, height, &scene).unwrap();
    assert_eq!((cw, ch), (88, 44));

    let px_at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * cw + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    };

    // Over the fine text stripes (base x in 14..38 -> cropped 10..34, base y in 12..34 -> cropped 10..32):
    // zero pure white (>190) or pure black (<30) pixels survive.
    let mut left_sum = 0.0_f64;
    let mut left_cnt = 0u64;
    for y in 10..32u32 {
        for x in 10..34u32 {
            let p = px_at(x, y);
            assert_eq!(p[3], 255);
            let luma = (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.0;
            assert!(
                p[0] <= 190 && p[0] >= 30,
                "fine stripe contrast leaked at ({x},{y}): {p:?}"
            );
            left_sum += luma;
            left_cnt += 1;
        }
    }
    let avg_left = left_sum / (left_cnt as f64);

    // Over the dark right panel (base x in 56..78 -> cropped 52..74, base y in 12..34 -> cropped 10..32):
    let mut right_sum = 0.0_f64;
    let mut right_cnt = 0u64;
    for y in 10..32u32 {
        for x in 52..74u32 {
            let p = px_at(x, y);
            assert_eq!(p[3], 255);
            let luma = (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.0;
            right_sum += luma;
            right_cnt += 1;
        }
    }
    let avg_right = right_sum / (right_cnt as f64);
    assert!(
        avg_left > avg_right + 30.0,
        "expected broad bright vs dark shapes to remain distinguishable: left={avg_left:.1}, right={avg_right:.1}"
    );

    let png = render_and_export(&base, width, height, &scene, ExportFormat::Png).unwrap();
    let chunks = png_chunk_types(&png);
    for forbidden in [b"eXIf", b"tEXt", b"zTXt", b"iTXt"] {
        assert!(!chunks.contains(forbidden));
    }
    let jpeg = render_and_export(
        &base,
        width,
        height,
        &scene,
        ExportFormat::Jpeg { quality: 90 },
    )
    .unwrap();
    assert!(!jpeg.is_empty());
}

#[test]
fn spray_renders_smooth_blended_radial_airbrush_without_gaps() {
    let base = solid_rgba(40, 40, [255, 255, 255, 255]);
    let mut scene = AnnotationScene::new(40, 40);
    scene.push_op(AnnotationOp::Spray {
        points: vec![Point::new(8.0, 20.0), Point::new(32.0, 20.0)],
        color: Color::RED,
        radius: 8.0,
    });

    let (out, w, h) = render_to_rgba(&base, 40, 40, &scene).unwrap();
    let pixel_at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    // Midpoint (20, 20) between sparse endpoints is smoothly tinted red with no gaps
    let center = pixel_at(20, 20);
    assert!(
        center[0] > 200 && center[1] < 180 && center[2] < 180,
        "expected continuous red airbrush center at (20,20), got {center:?}"
    );

    // Mid-radius (20, 25) is softer (higher G than center, yet visibly tinted below 255)
    let mid = pixel_at(20, 25);
    assert!(
        mid[1] > center[1] && mid[1] < 255,
        "expected smooth radial falloff at (20,25): center={center:?}, mid={mid:?}"
    );

    // Far outside soft_r remains untouched pure white
    assert_eq!(pixel_at(20, 38), [255, 255, 255, 255]);
    assert_eq!((w, h), (40, 40));
}
