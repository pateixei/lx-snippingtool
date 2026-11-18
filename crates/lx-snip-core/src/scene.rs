//! Retained-mode annotation and crop scene in image-pixel coordinates.

/// 2D point in image-pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Horizontal coordinate in pixels.
    pub x: f32,
    /// Vertical coordinate in pixels.
    pub y: f32,
}

impl Point {
    /// Creates a new [`Point`].
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Axis-aligned rectangle in image-pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge X coordinate.
    pub x: f32,
    /// Top edge Y coordinate.
    pub y: f32,
    /// Rectangle width.
    pub width: f32,
    /// Rectangle height.
    pub height: f32,
}

impl Rect {
    /// Creates a new [`Rect`].
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns a normalized copy with non-negative `width` and `height`.
    pub fn normalized(&self) -> Self {
        let (x, width) = if self.width < 0.0 {
            (self.x + self.width, -self.width)
        } else {
            (self.x, self.width)
        };
        let (y, height) = if self.height < 0.0 {
            (self.y + self.height, -self.height)
        } else {
            (self.y, self.height)
        };
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if `point` lies within the normalized rectangle (inclusive).
    pub fn contains(&self, point: Point) -> bool {
        let r = self.normalized();
        point.x >= r.x && point.x <= r.x + r.width && point.y >= r.y && point.y <= r.y + r.height
    }

    /// Clamps this rectangle to `[0, max_width] x [0, max_height]`.
    pub fn clamp(&self, max_width: f32, max_height: f32) -> Self {
        let r = self.normalized();
        let x0 = r.x.clamp(0.0, max_width);
        let y0 = r.y.clamp(0.0, max_height);
        let x1 = (r.x + r.width).clamp(0.0, max_width);
        let y1 = (r.y + r.height).clamp(0.0, max_height);
        Self {
            x: x0,
            y: y0,
            width: (x1 - x0).max(0.0),
            height: (y1 - y0).max(0.0),
        }
    }
}

/// Straight RGBA8 color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel.
    pub a: u8,
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Opaque red.
    pub const RED: Self = Self::rgb(220, 38, 38);
    /// Opaque yellow.
    pub const YELLOW: Self = Self::rgb(250, 204, 21);

    /// Creates an opaque RGB color.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Creates an RGBA color.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// Supported geometric shape annotation kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    /// Axis-aligned rectangle outline.
    Rectangle,
    /// Axis-aligned ellipse outline inscribed in the bounding box.
    Ellipse,
    /// Straight line with an arrowhead at `end`.
    Arrow,
    /// Straight line segment from `start` to `end`.
    Line,
}

/// A single retained-mode annotation operation in image-pixel coordinates.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationOp {
    /// Freehand opaque pen stroke.
    Pen {
        /// Polyline vertices in image-pixel coordinates.
        points: Vec<Point>,
        /// Stroke color.
        color: Color,
        /// Stroke width in pixels.
        width: f32,
    },
    /// Translucent highlighter stroke.
    Highlighter {
        /// Polyline vertices in image-pixel coordinates.
        points: Vec<Point>,
        /// Base stroke color.
        color: Color,
        /// Stroke width in pixels.
        width: f32,
        /// Opacity multiplier in `[0.0, 1.0]`.
        opacity: f32,
    },
    /// Geometric shape defined by two drag endpoints.
    Shape {
        /// Shape variant.
        kind: ShapeKind,
        /// Drag start point.
        start: Point,
        /// Drag end point.
        end: Point,
        /// Stroke color.
        color: Color,
        /// Stroke width in pixels.
        width: f32,
    },
    /// Destructive solid redaction box.
    RedactBox {
        /// Rectangle to overwrite with opaque black pixels on export.
        rect: Rect,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum SceneAction {
    PushOp(AnnotationOp),
    EraseOp {
        index: usize,
        op: AnnotationOp,
    },
    SetCrop {
        prev: Option<Rect>,
        next: Option<Rect>,
    },
}

/// Retained-mode annotation scene independent of UI toolkit widgets.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationScene {
    /// Base image width in pixels.
    pub base_width: u32,
    /// Base image height in pixels.
    pub base_height: u32,
    /// Optional crop rectangle applied during export.
    pub crop_rect: Option<Rect>,
    ops: Vec<AnnotationOp>,
    undo_stack: Vec<SceneAction>,
    redo_stack: Vec<SceneAction>,
}

impl AnnotationScene {
    /// Creates an empty [`AnnotationScene`] for an image of `base_width` x `base_height`.
    pub fn new(base_width: u32, base_height: u32) -> Self {
        Self {
            base_width,
            base_height,
            crop_rect: None,
            ops: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// Returns the currently active annotation operations in draw order.
    pub fn ops(&self) -> &[AnnotationOp] {
        &self.ops
    }

    /// Appends an annotation operation and records it on the undo stack.
    pub fn push_op(&mut self, op: AnnotationOp) {
        self.ops.push(op.clone());
        self.undo_stack.push(SceneAction::PushOp(op));
        self.redo_stack.clear();
    }

    /// Updates the crop rectangle and records the change on the undo stack.
    pub fn set_crop(&mut self, crop: Option<Rect>) {
        if self.crop_rect == crop {
            return;
        }
        let prev = self.crop_rect;
        self.crop_rect = crop;
        self.undo_stack
            .push(SceneAction::SetCrop { prev, next: crop });
        self.redo_stack.clear();
    }

    /// Returns `true` if there is an action available to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Returns `true` if there is an action available to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Undoes the most recent scene mutation, returning `true` if state changed.
    pub fn undo(&mut self) -> bool {
        let Some(action) = self.undo_stack.pop() else {
            return false;
        };

        match &action {
            SceneAction::PushOp(_) => {
                self.ops.pop();
            }
            SceneAction::EraseOp { index, op } => {
                let idx = (*index).min(self.ops.len());
                self.ops.insert(idx, op.clone());
            }
            SceneAction::SetCrop { prev, .. } => {
                self.crop_rect = *prev;
            }
        }

        self.redo_stack.push(action);
        true
    }

    /// Reapplies the most recently undone scene mutation, returning `true` if state changed.
    pub fn redo(&mut self) -> bool {
        let Some(action) = self.redo_stack.pop() else {
            return false;
        };

        match &action {
            SceneAction::PushOp(op) => {
                self.ops.push(op.clone());
            }
            SceneAction::EraseOp { index, .. } => {
                if *index < self.ops.len() {
                    self.ops.remove(*index);
                }
            }
            SceneAction::SetCrop { next, .. } => {
                self.crop_rect = *next;
            }
        }

        self.undo_stack.push(action);
        true
    }

    /// Performs stroke-level hit-testing at `point` within `radius` pixels and removes
    /// the topmost matching operation. Returns `true` if an operation was erased.
    pub fn erase_at(&mut self, point: Point, radius: f32) -> bool {
        let radius = radius.max(0.0);
        for idx in (0..self.ops.len()).rev() {
            if hit_test_op(&self.ops[idx], point, radius) {
                let op = self.ops.remove(idx);
                self.undo_stack
                    .push(SceneAction::EraseOp { index: idx, op });
                self.redo_stack.clear();
                return true;
            }
        }
        false
    }
}

fn hit_test_op(op: &AnnotationOp, point: Point, radius: f32) -> bool {
    match op {
        AnnotationOp::Pen { points, width, .. }
        | AnnotationOp::Highlighter { points, width, .. } => {
            let threshold = radius + (*width).max(1.0) * 0.5;
            hit_test_polyline(points, point, threshold)
        }
        AnnotationOp::Shape {
            kind,
            start,
            end,
            width,
            ..
        } => {
            let threshold = radius + (*width).max(1.0) * 0.5;
            match kind {
                ShapeKind::Line | ShapeKind::Arrow => {
                    distance_to_segment(point, *start, *end) <= threshold
                }
                ShapeKind::Rectangle => {
                    let rect =
                        Rect::new(start.x, start.y, end.x - start.x, end.y - start.y).normalized();
                    let tl = Point::new(rect.x, rect.y);
                    let tr = Point::new(rect.x + rect.width, rect.y);
                    let br = Point::new(rect.x + rect.width, rect.y + rect.height);
                    let bl = Point::new(rect.x, rect.y + rect.height);
                    distance_to_segment(point, tl, tr) <= threshold
                        || distance_to_segment(point, tr, br) <= threshold
                        || distance_to_segment(point, br, bl) <= threshold
                        || distance_to_segment(point, bl, tl) <= threshold
                }
                ShapeKind::Ellipse => {
                    let rect =
                        Rect::new(start.x, start.y, end.x - start.x, end.y - start.y).normalized();
                    hit_test_ellipse_outline(rect, point, threshold)
                }
            }
        }
        AnnotationOp::RedactBox { rect } => {
            let r = rect.normalized();
            let expanded = Rect::new(
                r.x - radius,
                r.y - radius,
                r.width + radius * 2.0,
                r.height + radius * 2.0,
            );
            expanded.contains(point)
        }
    }
}

fn hit_test_polyline(points: &[Point], target: Point, threshold: f32) -> bool {
    match points.len() {
        0 => false,
        1 => distance_sq(points[0], target) <= threshold * threshold,
        _ => points
            .windows(2)
            .any(|seg| distance_to_segment(target, seg[0], seg[1]) <= threshold),
    }
}

fn hit_test_ellipse_outline(rect: Rect, point: Point, threshold: f32) -> bool {
    let rx = rect.width * 0.5;
    let ry = rect.height * 0.5;
    if rx < 1e-3 && ry < 1e-3 {
        return distance_sq(Point::new(rect.x, rect.y), point) <= threshold * threshold;
    }
    if rx < 1e-3 {
        let top = Point::new(rect.x + rx, rect.y);
        let bottom = Point::new(rect.x + rx, rect.y + rect.height);
        return distance_to_segment(point, top, bottom) <= threshold;
    }
    if ry < 1e-3 {
        let left = Point::new(rect.x, rect.y + ry);
        let right = Point::new(rect.x + rect.width, rect.y + ry);
        return distance_to_segment(point, left, right) <= threshold;
    }

    let cx = rect.x + rx;
    let cy = rect.y + ry;
    // Sample 64 segments around the ellipse for accurate distance testing across aspect ratios.
    const STEPS: usize = 64;
    let mut prev = Point::new(cx + rx, cy);
    for i in 1..=STEPS {
        let theta = (i as f32 / STEPS as f32) * std::f32::consts::TAU;
        let curr = Point::new(cx + rx * theta.cos(), cy + ry * theta.sin());
        if distance_to_segment(point, prev, curr) <= threshold {
            return true;
        }
        prev = curr;
    }
    false
}

fn distance_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= 1e-6 {
        return distance_sq(p, a).sqrt();
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len_sq).clamp(0.0, 1.0);
    let proj = Point::new(a.x + t * dx, a.y + t * dy);
    distance_sq(p, proj).sqrt()
}

fn distance_sq(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}
