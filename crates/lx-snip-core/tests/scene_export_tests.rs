use image::GenericImageView;
use lx_snip_core::export::{render_and_export, render_to_rgba, ExportError, ExportFormat};
use lx_snip_core::scene::{AnnotationOp, AnnotationScene, Color, Point, Rect, ShapeKind};

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
    };
    let rect_shape = AnnotationOp::Shape {
        kind: ShapeKind::Rectangle,
        start: Point::new(20.0, 20.0),
        end: Point::new(80.0, 80.0),
        color: Color::RED,
        width: 2.0,
    };
    let ellipse = AnnotationOp::Shape {
        kind: ShapeKind::Ellipse,
        start: Point::new(100.0, 100.0),
        end: Point::new(160.0, 140.0),
        color: Color::YELLOW,
        width: 2.0,
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

    // Redacted pixel at base (7, 7) -> cropped (3, 3) must be opaque black [0, 0, 0, 255]
    assert_eq!(rgba.get_pixel(3, 3).0, [0, 0, 0, 255]);
    // Unredacted pixel at base (4, 4) -> cropped (0, 0) must still be green [10, 220, 30, 255]
    assert_eq!(rgba.get_pixel(0, 0).0, [10, 220, 30, 255]);
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
    assert!(matches!(
        render_to_rgba(&[0u8; 64], 4, 4, &bad_crop_scene),
        Err(ExportError::EmptyCrop)
    ));
}
