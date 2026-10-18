//! `cargo xtask icons` — значки FreeOS Icons из SVG в таблицу для `mini-ui`.
//!
//! # Зачем конвертер
//!
//! Роман прислал набор значков макета (FreeOS Icons 1.0: 68 линейных значков,
//! сетка 24×24, обводка 1,75, скруглённые концы). Ядро SVG не читает и читать
//! не будет: разбор чужого XML в ядре — это код, который нечем проверить, а
//! плавающей точки там нет вовсе. Поэтому разбор делается здесь, на хосте,
//! один раз: дуги и кривые Безье раскладываются в ломаные, координаты
//! переводятся в целые (1/16 клетки сетки), и в `mini-ui` ложится готовая
//! таблица — `crates/mini-ui/src/iconset_data.rs`. Её и обводит
//! `mini_ui::iconset` в любом размере и цвете.
//!
//! Исходник — `assets/icons/freeos-icons.svg` (спрайт из макета, без правок);
//! таблица — сгенерированный файл, и править её руками нельзя: следующий
//! запуск перепишет.
//!
//! # Что понимается
//!
//! Ровно то, что есть в наборе: `<path>` с командами `M L H V A C S Z` (в
//! обоих регистрах), `<rect>` со скруглением `rx`, `<circle>`. Заливок нет —
//! набор только обводкой. Встретив что-то другое, конвертер отказывает вслух,
//! а не пропускает молча: значок без половины штрихов хуже отсутствующего.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Единиц на клетку сетки 24×24.
const UNIT: f64 = 16.0;

/// Ломаная в единицах 1/16 клетки.
type Polyline = Vec<(i16, i16)>;

/// Перевести `assets/icons/freeos-icons.svg` в `crates/mini-ui/src/iconset_data.rs`.
pub fn generate(svg: &Path, out: &Path) -> Result<()> {
    let text = std::fs::read_to_string(svg).with_context(|| format!("не читается {}", svg.display()))?;
    let mut icons: Vec<(String, Vec<Polyline>)> = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("<symbol") {
        rest = &rest[at..];
        let head_end = rest.find('>').context("оборванный <symbol>")?;
        let head = &rest[..head_end];
        let id = attr(head, "id").context("<symbol> без id")?;
        let name = id.strip_prefix("fi-").unwrap_or(&id).to_string();
        let end = rest.find("</symbol>").context("нет </symbol>")?;
        let body = &rest[head_end + 1..end];
        let lines = shapes(body).with_context(|| format!("значок {name}"))?;
        icons.push((name, lines));
        rest = &rest[end + 9..];
    }
    if icons.is_empty() {
        bail!("в {} нет ни одного <symbol>", svg.display());
    }
    std::fs::write(out, emit(&icons)).with_context(|| format!("не пишется {}", out.display()))?;
    println!("icons: {} значков -> {}", icons.len(), out.display());
    Ok(())
}

/// Нарисовать все значки таблицы **кодом ядра** (`mini_ui::iconset`) в PNG:
/// сетка по 12 в ряд, каждый значок дважды — в 32 и в 16 точек, светлым на
/// тёмном, как в макете. Сверяется с макетом глазами — ради этого и сделано.
pub fn preview(out: &Path) -> Result<()> {
    use mini_ui::iconset::{self, Fi};
    use mini_ui::{Color, Surface};

    mini_ui::use_raw_format(mini_ui::PIXEL_RGB);
    let count = mini_ui::iconset_data::ICONS.len();
    let (columns, cell) = (12usize, 64u32);
    let rows = count.div_ceil(columns);
    let (width, height) = (columns as u32 * cell, rows as u32 * cell);
    let background = Color::rgb(0x16, 0x18, 0x1d);
    let mut surface = Surface::new(width, height, background).context("поверхность не создаётся")?;
    for (index, &icon) in Fi::ALL.iter().enumerate() {
        let (x, y) = ((index % columns) as i32 * cell as i32, (index / columns) as i32 * cell as i32);
        iconset::draw(&mut surface, icon, x + 4, y + 4, 32, Color::rgb(0xe9, 0xeb, 0xef), 255);
        iconset::draw(&mut surface, icon, x + 42, y + 42, 16, Color::rgb(0x8f, 0xb8, 0xff), 255);
    }
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for &pixel in surface.row(y) {
            let color = Color::from_pixel(pixel);
            rgb.extend_from_slice(&[color.r, color.g, color.b]);
        }
    }
    let file = std::fs::File::create(out).with_context(|| format!("не пишется {}", out.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    println!("icons: предпросмотр -> {}", out.display());
    Ok(())
}

/// Значение атрибута `name="…"` в теге.
fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let at = tag.find(&key)? + key.len();
    let end = tag[at..].find('"')? + at;
    Some(tag[at..end].to_string())
}

fn num(tag: &str, name: &str) -> f64 {
    attr(tag, name).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

/// Все фигуры тела `<symbol>` — ломаными.
fn shapes(body: &str) -> Result<Vec<Polyline>> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find('<') {
        rest = &rest[at..];
        let end = rest.find('>').context("оборванный тег")?;
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        if tag.starts_with("</") {
            continue;
        }
        let kind: String = tag[1..].chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        match kind.as_str() {
            "path" => out.extend(path(&attr(tag, "d").context("<path> без d")?)?),
            "rect" => out.push(rect(num(tag, "x"), num(tag, "y"), num(tag, "width"), num(tag, "height"), num(tag, "rx"))),
            "circle" => out.push(ellipse(num(tag, "cx"), num(tag, "cy"), num(tag, "r"), num(tag, "r"))),
            "ellipse" => out.push(ellipse(num(tag, "cx"), num(tag, "cy"), num(tag, "rx"), num(tag, "ry"))),
            "line" => out.push(vec![point(num(tag, "x1"), num(tag, "y1")), point(num(tag, "x2"), num(tag, "y2"))]),
            other => bail!("тег <{other}> конвертер не понимает"),
        }
    }
    Ok(out)
}

fn point(x: f64, y: f64) -> (i16, i16) {
    ((x * UNIT).round() as i16, (y * UNIT).round() as i16)
}

/// Прямоугольник со скруглёнными углами: дуги по четверти окружности.
fn rect(x: f64, y: f64, w: f64, h: f64, rx: f64) -> Polyline {
    let r = rx.min(w / 2.0).min(h / 2.0);
    let mut pts = Vec::new();
    let corner = |pts: &mut Vec<(f64, f64)>, cx: f64, cy: f64, start: f64| {
        let steps = arc_steps(r, std::f64::consts::FRAC_PI_2);
        for i in 0..=steps {
            let a = start + std::f64::consts::FRAC_PI_2 * f64::from(i) / f64::from(steps);
            pts.push((cx + r * a.cos(), cy + r * a.sin()));
        }
    };
    if r <= 0.0 {
        pts.extend([(x, y), (x + w, y), (x + w, y + h), (x, y + h)]);
    } else {
        use std::f64::consts::PI;
        corner(&mut pts, x + w - r, y + r, -PI / 2.0);
        corner(&mut pts, x + w - r, y + h - r, 0.0);
        corner(&mut pts, x + r, y + h - r, PI / 2.0);
        corner(&mut pts, x + r, y + r, PI);
    }
    pts.push(pts[0]);
    dedup(pts.into_iter().map(|(x, y)| point(x, y)).collect())
}

fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Polyline {
    let steps = arc_steps(rx.max(ry), 2.0 * std::f64::consts::PI);
    let pts = (0..=steps)
        .map(|i| {
            let a = 2.0 * std::f64::consts::PI * f64::from(i) / f64::from(steps);
            point(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect();
    dedup(pts)
}

/// На сколько отрезков резать дугу: отклонение хорды от дуги — не больше
/// пятидесятой клетки, но не меньше четырёх отрезков на четверть.
fn arc_steps(r: f64, sweep: f64) -> u32 {
    let tolerance = 0.02;
    let step = if r > tolerance { 2.0 * (1.0 - tolerance / r).acos() } else { sweep };
    ((sweep.abs() / step.max(0.05)).ceil() as u32).max((sweep.abs() / (std::f64::consts::PI / 8.0)).ceil() as u32).max(1)
}

/// Соседние совпавшие точки — лишние (кроме ломаной из одной точки: это точка).
fn dedup(mut pts: Polyline) -> Polyline {
    pts.dedup();
    pts
}

/// Разобрать атрибут `d`.
fn path(d: &str) -> Result<Vec<Polyline>> {
    let tokens = tokenize(d)?;
    let mut out = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    // Вторая управляющая точка прошлой кривой — для `S`.
    let mut last_ctrl: Option<(f64, f64)> = None;
    let mut i = 0;
    let mut command = 'M';
    let flush = |current: &mut Vec<(f64, f64)>, out: &mut Vec<Polyline>| {
        if !current.is_empty() {
            out.push(dedup(current.iter().map(|&(x, y)| point(x, y)).collect()));
            current.clear();
        }
    };
    while i < tokens.len() {
        if let Token::Cmd(c) = tokens[i] {
            command = c;
            i += 1;
            if c == 'Z' || c == 'z' {
                current.push((sx, sy));
                flush(&mut current, &mut out);
                x = sx;
                y = sy;
                last_ctrl = None;
                continue;
            }
        }
        let rel = command.is_ascii_lowercase();
        let now = command;
        let mut take = |n: usize| -> Result<Vec<f64>> {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                match tokens.get(i) {
                    Some(Token::Num(value)) => v.push(*value),
                    _ => bail!("команде {now} не хватило чисел"),
                }
                i += 1;
            }
            Ok(v)
        };
        match command.to_ascii_uppercase() {
            'M' => {
                let v = take(2)?;
                flush(&mut current, &mut out);
                if rel {
                    x += v[0];
                    y += v[1];
                } else {
                    x = v[0];
                    y = v[1];
                }
                sx = x;
                sy = y;
                current.push((x, y));
                // Следующие пары после M — это L.
                command = if rel { 'l' } else { 'L' };
                last_ctrl = None;
            }
            'L' => {
                let v = take(2)?;
                if rel {
                    x += v[0];
                    y += v[1];
                } else {
                    x = v[0];
                    y = v[1];
                }
                current.push((x, y));
                last_ctrl = None;
            }
            'H' => {
                let v = take(1)?;
                x = if rel { x + v[0] } else { v[0] };
                current.push((x, y));
                last_ctrl = None;
            }
            'V' => {
                let v = take(1)?;
                y = if rel { y + v[0] } else { v[0] };
                current.push((x, y));
                last_ctrl = None;
            }
            'C' | 'S' => {
                let smooth = command.to_ascii_uppercase() == 'S';
                let v = take(if smooth { 4 } else { 6 })?;
                let (bx, by) = (if rel { x } else { 0.0 }, if rel { y } else { 0.0 });
                let (c1, c2, end) = if smooth {
                    let c1 = last_ctrl.map_or((x, y), |(lx, ly)| (2.0 * x - lx, 2.0 * y - ly));
                    (c1, (bx + v[0], by + v[1]), (bx + v[2], by + v[3]))
                } else {
                    ((bx + v[0], by + v[1]), (bx + v[2], by + v[3]), (bx + v[4], by + v[5]))
                };
                let steps = 12;
                for k in 1..=steps {
                    let t = f64::from(k) / f64::from(steps);
                    let u = 1.0 - t;
                    let px = u * u * u * x + 3.0 * u * u * t * c1.0 + 3.0 * u * t * t * c2.0 + t * t * t * end.0;
                    let py = u * u * u * y + 3.0 * u * u * t * c1.1 + 3.0 * u * t * t * c2.1 + t * t * t * end.1;
                    current.push((px, py));
                }
                last_ctrl = Some(c2);
                x = end.0;
                y = end.1;
            }
            'A' => {
                let v = take(7)?;
                let (ex, ey) = if rel { (x + v[5], y + v[6]) } else { (v[5], v[6]) };
                arc(&mut current, (x, y), v[0], v[1], v[2], v[3] != 0.0, v[4] != 0.0, (ex, ey));
                x = ex;
                y = ey;
                last_ctrl = None;
            }
            other => bail!("команду {other} конвертер не понимает"),
        }
    }
    flush(&mut current, &mut out);
    Ok(out)
}

/// Дуга эллипса SVG (W3C, F.6.5: из концов — в центр) — точками в `current`.
#[allow(clippy::too_many_arguments)]
fn arc(
    current: &mut Vec<(f64, f64)>,
    from: (f64, f64),
    rx: f64,
    ry: f64,
    rotation: f64,
    large: bool,
    sweep: bool,
    to: (f64, f64),
) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || from == to {
        current.push(to);
        return;
    }
    let phi = rotation.to_radians();
    let (cos, sin) = (phi.cos(), phi.sin());
    let dx = (from.0 - to.0) / 2.0;
    let dy = (from.1 - to.1) / 2.0;
    let x1 = cos * dx + sin * dy;
    let y1 = -sin * dx + cos * dy;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cx1 = coef * rx * y1 / ry;
    let cy1 = -coef * ry * x1 / rx;
    let cx = cos * cx1 - sin * cy1 + (from.0 + to.0) / 2.0;
    let cy = sin * cx1 + cos * cy1 + (from.1 + to.1) / 2.0;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 { -a } else { a }
    };
    let theta = angle(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut delta = angle((x1 - cx1) / rx, (y1 - cy1) / ry, (-x1 - cx1) / rx, (-y1 - cy1) / ry);
    if !sweep && delta > 0.0 {
        delta -= 2.0 * std::f64::consts::PI;
    } else if sweep && delta < 0.0 {
        delta += 2.0 * std::f64::consts::PI;
    }
    let steps = arc_steps(rx.max(ry), delta);
    for k in 1..=steps {
        let t = theta + delta * f64::from(k) / f64::from(steps);
        let (px, py) = (rx * t.cos(), ry * t.sin());
        current.push((cos * px - sin * py + cx, sin * px + cos * py + cy));
    }
}

enum Token {
    Cmd(char),
    Num(f64),
}

/// Разрезать `d` на команды и числа (`.5.5`, `-1-2`, `1e-3` — как в SVG).
fn tokenize(d: &str) -> Result<Vec<Token>> {
    let bytes = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            out.push(Token::Cmd(c));
            i += 1;
        } else if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' {
            let start = i;
            i += 1;
            let mut dot = c == '.';
            while i < bytes.len() {
                let n = bytes[i] as char;
                if n.is_ascii_digit() {
                    i += 1;
                } else if n == '.' && !dot {
                    dot = true;
                    i += 1;
                } else if (n == 'e' || n == 'E') && i + 1 < bytes.len() {
                    i += 1;
                    if bytes[i] == b'-' || bytes[i] == b'+' {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            let text = &d[start..i];
            out.push(Token::Num(text.parse().with_context(|| format!("не число: {text}"))?));
        } else {
            i += 1;
        }
    }
    Ok(out)
}

/// `home` → `Home`, `chevron-left` → `ChevronLeft`.
fn variant(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |c| c.to_ascii_uppercase().to_string() + chars.as_str())
        })
        .collect()
}

/// Текст сгенерированного модуля.
fn emit(icons: &[(String, Vec<Polyline>)]) -> String {
    let mut s = String::new();
    s.push_str(
        "//! Значки FreeOS Icons 1.0 ломаными — СГЕНЕРИРОВАНО `cargo xtask icons` из\n\
         //! `assets/icons/freeos-icons.svg`. Не править руками.\n\
         //!\n\
         //! Координаты — в 1/16 клетки сетки 24×24 (квадрат 384×384).\n\n",
    );
    s.push_str("/// Значок набора.\n#[derive(Clone, Copy, PartialEq, Eq, Debug)]\n#[repr(u8)]\npub enum Fi {\n");
    for (name, _) in icons {
        let _ = writeln!(s, "    /// `fi-{name}`.\n    {},", variant(name));
    }
    s.push_str("}\n\n");
    let _ = writeln!(s, "impl Fi {{\n    /// Все значки по порядку таблицы.\n    pub const ALL: [Fi; {}] = [", icons.len());
    for (name, _) in icons {
        let _ = writeln!(s, "        Fi::{},", variant(name));
    }
    s.push_str("    ];\n}\n\n");
    let mut points = Vec::new();
    let mut strokes = Vec::new();
    let mut ranges = Vec::new();
    for (_, lines) in icons {
        let first = strokes.len();
        for line in lines {
            strokes.push((points.len(), line.len()));
            points.extend(line.iter().copied());
        }
        ranges.push((first, strokes.len() - first));
    }
    let _ = writeln!(s, "/// Точки всех штрихов подряд: x, y.\npub static POINTS: [(i16, i16); {}] = [", points.len());
    for chunk in points.chunks(8) {
        s.push_str("   ");
        for (x, y) in chunk {
            let _ = write!(s, " ({x}, {y}),");
        }
        s.push('\n');
    }
    s.push_str("];\n\n");
    let _ = writeln!(s, "/// Штрихи: начало в [`POINTS`] и число точек.\npub static STROKES: [(u16, u16); {}] = [", strokes.len());
    for chunk in strokes.chunks(8) {
        s.push_str("   ");
        for (start, len) in chunk {
            let _ = write!(s, " ({start}, {len}),");
        }
        s.push('\n');
    }
    s.push_str("];\n\n");
    let _ = writeln!(s, "/// Штрихи значка: начало в [`STROKES`] и число, по порядку [`Fi`].\npub static ICONS: [(u16, u16); {}] = [", ranges.len());
    for (name, (first, count)) in icons.iter().map(|(n, _)| n).zip(ranges) {
        let _ = writeln!(s, "    ({first}, {count}), // {name}");
    }
    s.push_str("];\n");
    s
}
