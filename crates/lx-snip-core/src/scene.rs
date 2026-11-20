//! Módulo: `scene`
//! Objetivos: Manter a cena retida de anotações e recorte em coordenadas de pixel com suporte a desfazer/refazer, estilos de linha, spray, curvas Catmull-Rom/Bézier, formas e hit-testing de borracha.
//! Principais itens: `Point`, `Rect`, `Color`, `LineStyle`, `ShapeKind`, `AnnotationOp`, `AnnotationScene`, `catmull_rom_spline_points` e `spray_stamp_points`.
//! Última alteração: 2026-11-19

use serde::{Deserialize, Serialize};

/// Ponto 2D em coordenadas de pixel da imagem capturada.
/// Armazena: coordenadas `x` e `y` | Principais métodos: `new`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Coordenada horizontal em pixels.
    pub x: f32,
    /// Coordenada vertical em pixels.
    pub y: f32,
}

impl Point {
    /// Cria um novo ponto `Point` nas coordenadas especificadas.
    /// Input: coordenadas `x` e `y` em pixels | Output: instância de `Point`.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Retângulo alinhado aos eixos em coordenadas de pixel da imagem.
/// Armazena: origem `x`/`y` e dimensões `width`/`height` | Principais métodos: `new`, `normalized`, `contains`, `clamp`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    /// Coordenada X da borda esquerda.
    pub x: f32,
    /// Coordenada Y da borda superior.
    pub y: f32,
    /// Largura do retângulo em pixels.
    pub width: f32,
    /// Altura do retângulo em pixels.
    pub height: f32,
}

impl Rect {
    /// Cria um novo retângulo `Rect` a partir de origem e dimensões.
    /// Input: `x`, `y`, `width` e `height` | Output: instância de `Rect`.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Retorna uma cópia normalizada garantindo largura e altura não negativas.
    /// Input: `&self` | Output: `Rect` com `width >= 0.0` e `height >= 0.0`.
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

    /// Verifica se um ponto está contido dentro do retângulo normalizado (bordas inclusas).
    /// Input: `&self` e `point` | Output: `true` se o ponto estiver dentro do retângulo.
    pub fn contains(&self, point: Point) -> bool {
        let r = self.normalized();
        point.x >= r.x && point.x <= r.x + r.width && point.y >= r.y && point.y <= r.y + r.height
    }

    /// Limita as coordenadas do retângulo aos limites `[0, max_width] x [0, max_height]`.
    /// Input: `&self`, `max_width` e `max_height` | Output: `Rect` normalizado e recortado.
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

/// Representa uma cor RGBA8 não pré-multiplicada usada nas anotações.
/// Armazena: canais `r`, `g`, `b` e `a` | Principais métodos: `rgb`, `rgba`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    /// Canal vermelho.
    pub r: u8,
    /// Canal verde.
    pub g: u8,
    /// Canal azul.
    pub b: u8,
    /// Canal alfa (transparência).
    pub a: u8,
}

impl Color {
    /// Preto opaco padrão.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Branco opaco padrão.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Vermelho opaco padrão.
    pub const RED: Self = Self::rgb(220, 38, 38);
    /// Amarelo opaco padrão.
    pub const YELLOW: Self = Self::rgb(250, 204, 21);

    /// Cria uma cor RGB totalmente opaca (`a = 255`).
    /// Input: canais `r`, `g` e `b` | Output: instância opaca de `Color`.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Cria uma cor RGBA com transparência explícita.
    /// Input: canais `r`, `g`, `b` e `a` | Output: instância de `Color`.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// Estilo de traçado de linha aplicado a caneta à mão livre e formas geométricas.
/// Armazena: variantes `Solid`, `Dotted` e `Dashed` | Principais métodos: `Default::default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LineStyle {
    /// Linha contínua sólida.
    #[default]
    Solid,
    /// Linha pontilhada (pontos espaçados).
    Dotted,
    /// Linha tracejada (segmentos espaçados).
    Dashed,
}

/// Enumera as formas geométricas vetoriais suportadas pelo editor.
/// Armazena: variantes `Rectangle`, `Ellipse`, `Arrow`, `DoubleArrow` e `Line` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapeKind {
    /// Contorno de retângulo alinhado aos eixos.
    Rectangle,
    /// Contorno de elipse/esfera inscrita no retângulo delimitador.
    Ellipse,
    /// Segmento reto com ponta de seta em `end`.
    Arrow,
    /// Segmento reto com pontas de seta nas duas extremidades (`start` e `end`).
    DoubleArrow,
    /// Segmento reto simples entre `start` e `end`.
    Line,
}

/// Descreve uma operação retida individual de desenho ou redação na cena.
/// Armazena: variantes `Pen`, `Highlighter`, `Spray`, `CurvedLine`, `Shape` e `RedactBox` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnnotationOp {
    /// Traço opaco à mão livre com caneta e estilo de linha configurável.
    Pen {
        /// Lista de vértices da polilinha em pixels da imagem.
        points: Vec<Point>,
        /// Cor do traço.
        color: Color,
        /// Espessura do traço em pixels.
        width: f32,
        /// Estilo de linha (`Solid`, `Dotted` ou `Dashed`).
        #[serde(default)]
        style: LineStyle,
    },
    /// Traço translúcido de marca-texto.
    Highlighter {
        /// Lista de vértices da polilinha em pixels da imagem.
        points: Vec<Point>,
        /// Cor base do marca-texto.
        color: Color,
        /// Espessura do traço em pixels.
        width: f32,
        /// Multiplicador de opacidade em `[0.0, 1.0]`.
        opacity: f32,
    },
    /// Pincel aerógrafo (spray) dispersando partículas em torno da trajetória.
    Spray {
        /// Lista de centros da trajetória do jato em pixels da imagem.
        points: Vec<Point>,
        /// Cor das partículas do spray.
        color: Color,
        /// Raio de dispersão do jato em pixels.
        radius: f32,
    },
    /// Linha curva suavizada por interpolação spline cúbica Catmull-Rom / Bézier sobre pontos de passagem.
    CurvedLine {
        /// Lista de amostras brutas ou pontos de passagem em pixels da imagem.
        points: Vec<Point>,
        /// Cor da curva.
        color: Color,
        /// Espessura do traço em pixels.
        width: f32,
        /// Estilo de linha (`Solid`, `Dotted` ou `Dashed`).
        #[serde(default)]
        style: LineStyle,
    },
    /// Forma geométrica definida por ponto inicial e final do arrasto.
    Shape {
        /// Tipo de forma geométrica.
        kind: ShapeKind,
        /// Ponto inicial do arrasto.
        start: Point,
        /// Ponto final do arrasto.
        end: Point,
        /// Cor da borda da forma.
        color: Color,
        /// Espessura da linha em pixels.
        width: f32,
        /// Estilo de linha do contorno (`Solid`, `Dotted` ou `Dashed`).
        #[serde(default)]
        style: LineStyle,
    },
    /// Caixa de vidro fosco fumê (desfoque forte + camada escura 50% + dispersão estocástica em toda a área e bordas).
    RedactBox {
        /// Retângulo onde o efeito de vidro fosco fumê será aplicado na exportação.
        rect: Rect,
    },
}

/// Registra mutações reversíveis da cena para as pilhas de `undo` e `redo`.
/// Armazena: variantes `PushOp`, `EraseOp` e `SetCrop` | Principais métodos: nenhum (tipo de dados).
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

/// Mantém a cena retida de anotações, recorte e pilhas de histórico independentes de UI.
/// Armazena: dimensões `base_width`/`base_height`, `crop_rect`, lista `ops` e pilhas `undo_stack`/`redo_stack` | Principais métodos: `new`, `push_op`, `set_crop`, `undo`, `redo`, `erase_at`.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationScene {
    /// Largura da imagem base em pixels.
    pub base_width: u32,
    /// Altura da imagem base em pixels.
    pub base_height: u32,
    /// Retângulo opcional de recorte aplicado durante a exportação.
    pub crop_rect: Option<Rect>,
    ops: Vec<AnnotationOp>,
    undo_stack: Vec<SceneAction>,
    redo_stack: Vec<SceneAction>,
}

impl AnnotationScene {
    /// Cria uma cena `AnnotationScene` vazia para uma imagem de `base_width` x `base_height`.
    /// Input: `base_width` e `base_height` | Output: instância inicial de `AnnotationScene`.
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

    /// Retorna as operações ativas atualmente na cena em ordem de desenho.
    /// Input: `&self` | Output: slice `&[AnnotationOp]`.
    pub fn ops(&self) -> &[AnnotationOp] {
        &self.ops
    }

    /// Adiciona uma operação à cena, registra em `undo_stack` e limpa a pilha de refazer.
    /// Input: `&mut self` e operação `op` | Output: cena atualizada.
    pub fn push_op(&mut self, op: AnnotationOp) {
        self.ops.push(op.clone());
        self.undo_stack.push(SceneAction::PushOp(op));
        self.redo_stack.clear();
    }

    /// Atualiza o retângulo de recorte registrando a mudança para desfazer/refazer.
    /// Input: `&mut self` e `crop` opcional | Output: cena atualizada.
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

    /// Indica se existe ao menos uma ação disponível para desfazer.
    /// Input: `&self` | Output: `true` quando `undo_stack` não está vazia.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Indica se existe ao menos uma ação disponível para refazer.
    /// Input: `&self` | Output: `true` quando `redo_stack` não está vazia.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Desfaz a mutação mais recente da cena movendo-a para `redo_stack`.
    /// Input: `&mut self` | Output: `true` se alguma ação foi desfeita.
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

    /// Reaplica a última ação desfeita restaurando-a para `undo_stack`.
    /// Input: `&mut self` | Output: `true` se alguma ação foi reaplicada.
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

    /// Testa colisão de traço no ponto indicado e remove a operação mais alta correspondente.
    /// Input: `&mut self`, `point` em pixels e `radius` de tolerância | Output: `true` se apagou uma operação.
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

/// Verifica se o ponto de borracha atinge uma operação específica dentro do raio informado.
/// Input: operação `op`, `point` e `radius` | Output: `true` em caso de interseção.
fn hit_test_op(op: &AnnotationOp, point: Point, radius: f32) -> bool {
    match op {
        AnnotationOp::Pen { points, width, .. }
        | AnnotationOp::Highlighter { points, width, .. } => {
            let threshold = radius + (*width).max(1.0) * 0.5;
            hit_test_polyline(points, point, threshold)
        }
        AnnotationOp::Spray {
            points,
            radius: spray_r,
            ..
        } => {
            let threshold = radius + (*spray_r).max(2.0);
            hit_test_polyline(points, point, threshold)
        }
        AnnotationOp::CurvedLine { points, width, .. } => {
            let threshold = radius + (*width).max(1.0) * 0.5;
            let spline = catmull_rom_spline_points(points);
            hit_test_polyline(&spline, point, threshold)
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
                ShapeKind::Line | ShapeKind::Arrow | ShapeKind::DoubleArrow => {
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

/// Gera uma polilinha densamente interpolada por spline cúbica Catmull-Rom / Bézier passando pelos nós principais do caminho.
/// Input: slice de pontos `raw` | Output: `Vec<Point>` com a curva suavizada.
pub fn catmull_rom_spline_points(raw: &[Point]) -> Vec<Point> {
    if raw.len() <= 2 {
        return raw.to_vec();
    }

    let (min_knot_dist_sq, end_snap_sq) = if raw.len() <= 24 {
        (2.0 * 2.0, 2.0 * 2.0)
    } else {
        (18.0 * 18.0, 8.0 * 8.0)
    };
    let mut knots = Vec::with_capacity(raw.len());
    knots.push(raw[0]);
    for &pt in &raw[1..raw.len() - 1] {
        let last = *knots.last().unwrap_or(&raw[0]);
        if distance_sq(pt, last) >= min_knot_dist_sq {
            knots.push(pt);
        }
    }
    let end_pt = raw[raw.len() - 1];
    let knots_len = knots.len();
    if let Some(last) = knots.last_mut() {
        let d_sq = distance_sq(*last, end_pt);
        if knots_len > 1 && d_sq < end_snap_sq {
            *last = end_pt;
        } else if d_sq > 1e-4 {
            knots.push(end_pt);
        }
    }

    if knots.len() == 2 && raw.len() >= 3 {
        let mid = raw[raw.len() / 2];
        if distance_sq(mid, knots[0]) > 4.0 && distance_sq(mid, knots[1]) > 4.0 {
            knots.insert(1, mid);
        }
    }

    if knots.len() <= 2 {
        return knots;
    }

    const SUBDIVISIONS: usize = 16;
    let n = knots.len();
    let mut out = Vec::with_capacity((n - 1) * SUBDIVISIONS + 1);
    out.push(knots[0]);

    for i in 0..(n - 1) {
        let p0 = if i == 0 { knots[0] } else { knots[i - 1] };
        let p1 = knots[i];
        let p2 = knots[i + 1];
        let p3 = if i + 2 < n {
            knots[i + 2]
        } else {
            knots[n - 1]
        };

        let b1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
        let b2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);

        for step in 1..=SUBDIVISIONS {
            let t = (step as f32) / (SUBDIVISIONS as f32);
            let inv = 1.0 - t;
            let c0 = inv * inv * inv;
            let c1 = 3.0 * inv * inv * t;
            let c2 = 3.0 * inv * t * t;
            let c3 = t * t * t;
            out.push(Point::new(
                c0 * p1.x + c1 * b1.x + c2 * b2.x + c3 * p2.x,
                c0 * p1.y + c1 * b1.y + c2 * b2.y + c3 * p2.y,
            ));
        }
    }

    out
}

/// Reamostra uma trajetória de pontos em carimbos uniformemente espaçados para aerógrafo suave (spray contínuo).
/// Input: slice de pontos `points` e espaçamento `spacing` em pixels | Output: `Vec<Point>` com centros de carimbo distribuídos ao longo do traço.
pub fn spray_stamp_points(points: &[Point], spacing: f32) -> Vec<Point> {
    if points.is_empty() {
        return Vec::new();
    }
    if points.len() == 1 {
        return vec![points[0]];
    }

    let step = spacing.max(1.0);
    let mut out = Vec::with_capacity(points.len());
    out.push(points[0]);
    let mut residual = 0.0_f32;

    for seg in points.windows(2) {
        let a = seg[0];
        let b = seg[1];
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len <= 1e-4 {
            continue;
        }
        let mut d = step - residual;
        while d <= seg_len {
            let t = (d / seg_len).clamp(0.0, 1.0);
            out.push(Point::new(a.x + dx * t, a.y + dy * t));
            d += step;
        }
        residual = seg_len - (d - step);
    }

    out
}

/// Gera coordenadas determinísticas de partículas de aerógrafo (spray) em torno de um centro `(cx, cy)`.
/// Input: índice `step_idx`, centro `center` e raio `radius` | Output: `Vec<Point>` com as gotículas geradas.
pub fn spray_particles_for_point(step_idx: usize, center: Point, radius: f32) -> Vec<Point> {
    let r = radius.max(2.0);
    let count = ((r * 1.4).round() as usize).clamp(10, 42);
    let mut out = Vec::with_capacity(count);
    let base_seed = (step_idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ ((center.x.to_bits() as u64) << 32)
        ^ (center.y.to_bits() as u64);
    for i in 0..count {
        let mut z = base_seed.wrapping_add((i as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let u1 = ((z & 0xFFFF) as f32) / 65535.0;
        let u2 = (((z >> 16) & 0xFFFF) as f32) / 65535.0;
        let dist = u1.sqrt() * r;
        let angle = u2 * std::f32::consts::TAU;
        out.push(Point::new(
            center.x + dist * angle.cos(),
            center.y + dist * angle.sin(),
        ));
    }
    out
}

/// Testa se o ponto alvo está próximo de qualquer segmento de uma polilinha.
/// Input: vértices `points`, ponto `target` e distância `threshold` | Output: `true` se colidir.
fn hit_test_polyline(points: &[Point], target: Point, threshold: f32) -> bool {
    match points.len() {
        0 => false,
        1 => distance_sq(points[0], target) <= threshold * threshold,
        _ => points
            .windows(2)
            .any(|seg| distance_to_segment(target, seg[0], seg[1]) <= threshold),
    }
}

/// Aproxima o contorno de uma elipse por segmentos e testa distância até `point`.
/// Input: retângulo delimitador `rect`, `point` e `threshold` | Output: `true` se atingir a borda.
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

/// Calcula a menor distância euclidiana entre o ponto `p` e o segmento `a`-`b`.
/// Input: pontos `p`, `a` e `b` | Output: distância escalar `f32` em pixels.
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

/// Calcula o quadrado da distância euclidiana entre dois pontos.
/// Input: pontos `a` e `b` | Output: distância ao quadrado `f32`.
fn distance_sq(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}
