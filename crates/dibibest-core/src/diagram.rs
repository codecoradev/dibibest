//! The ERD as an SVG image (#60, #64). One renderer for every surface: the web
//! app (through WASM), the MCP servers and the CLI.
//!
//! Input is an ERD-JSON document (`nodes[].position` used as is; tables without
//! a position get the grid layout). Geometry mirrors the canvas table node in
//! `apps/web`: fixed 220px cards, a 33px header and 24px rows. Pure: no I/O.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use serde_json::Value;

use crate::erdjson::{GRID_CELL_H, GRID_CELL_W};
use crate::{convert_explicit, detect_format, CoreError, FormatId};

const W: f64 = 220.0;
const HEADER: f64 = 33.0;
const ROW: f64 = 24.0;
const PAD_Y: f64 = 4.0;
const MARGIN: f64 = 40.0;

/// Colour scheme of the image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Theme {
    /// The app's look.
    #[default]
    Dark,
    /// For documents and white backgrounds.
    Light,
}

impl Theme {
    /// `dark` | `light`; anything else is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

struct Palette {
    bg: &'static str,
    card: &'static str,
    head: &'static str,
    border: &'static str,
    line: &'static str,
    text: &'static str,
    text2: &'static str,
    text3: &'static str,
    accent: &'static str,
    pk: &'static str,
    fk: &'static str,
}

const DARK: Palette = Palette {
    bg: "#0b0d10",
    card: "#171b23",
    head: "#1b202a",
    border: "#262d3a",
    line: "#1c222d",
    text: "#e8ecf2",
    text2: "#9aa4b2",
    text3: "#8b95a5",
    accent: "#4f8cff",
    pk: "#f5b544",
    fk: "#b48cff",
};

const LIGHT: Palette = Palette {
    bg: "#ffffff",
    card: "#ffffff",
    head: "#f4f6fa",
    border: "#d5dbe5",
    line: "#e5e9f0",
    text: "#111827",
    text2: "#374151",
    text3: "#6b7280",
    accent: "#2563eb",
    pk: "#b7791f",
    fk: "#7c3aed",
};

const SANS: &str = "'IBM Plex Sans', system-ui, -apple-system, 'Segoe UI', sans-serif";
const MONO: &str = "'JetBrains Mono', ui-monospace, 'SFMono-Regular', Menlo, Consolas, monospace";

/// What to draw.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    pub theme: Theme,
    /// Table ids to leave out (the explorer's hidden tables), with their refs.
    pub hidden: HashSet<String>,
}

/// A standalone SVG document and its size in CSS pixels.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Svg {
    pub svg: String,
    pub width: u32,
    pub height: u32,
}

/// XML-escape text and attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Cut to `max` characters with an ellipsis: the card has a fixed width.
fn fit(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep: String = s.chars().take(max.saturating_sub(1).max(1)).collect();
    format!("{keep}…")
}

/// A number for SVG: integers without a fraction, otherwise one decimal.
fn n(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 {
        format!("{}", r as i64)
    } else {
        format!("{r:.1}")
    }
}

struct Col {
    name: String,
    ty: String,
    pk: bool,
    fk: bool,
}

struct Node {
    id: String,
    name: String,
    x: f64,
    y: f64,
    cols: Vec<Col>,
}

impl Node {
    fn height(&self) -> f64 {
        1.0 + HEADER + PAD_Y + self.cols.len() as f64 * ROW + PAD_Y + 1.0
    }
    /// Vertical centre of column `name` (first row when unknown).
    fn row_y(&self, name: &str) -> f64 {
        let i = self.cols.iter().position(|c| c.name == name).unwrap_or(0) as f64;
        self.y + 1.0 + HEADER + PAD_Y + i * ROW + ROW / 2.0
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// xyflow's default bezier between a right-side source and a left-side target handle.
fn bezier(sx: f64, sy: f64, tx: f64, ty: f64) -> String {
    let off = |d: f64| {
        if d >= 0.0 {
            0.5 * d
        } else {
            0.25 * 25.0 * (-d).sqrt()
        }
    };
    let c1 = sx + off(tx - sx);
    let c2 = tx - off(tx - sx);
    format!(
        "M{},{} C{},{} {},{} {},{}",
        n(sx),
        n(sy),
        n(c1),
        n(sy),
        n(c2),
        n(ty),
        n(tx),
        n(ty)
    )
}

/// Draw an ERD-JSON document. `None` when there is no table to draw.
pub fn render_svg(doc: &Value, opts: &RenderOptions) -> Option<Svg> {
    let p = match opts.theme {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    };
    let raw: Vec<&Value> = doc
        .get("nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|v| !opts.hidden.contains(s(v, "id")))
                .collect()
        })
        .unwrap_or_default();
    if raw.is_empty() {
        return None;
    }
    let cols_per_row = ((raw.len() as f64).sqrt().ceil() as usize).max(1);
    let mut nodes: Vec<Node> = raw
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let pos = v.get("position").filter(|p| p.is_object());
            let (x, y) = match pos {
                Some(p) => (
                    p.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                    p.get("y").and_then(Value::as_f64).unwrap_or(0.0),
                ),
                None => (
                    ((i % cols_per_row) as i64 * GRID_CELL_W) as f64,
                    ((i / cols_per_row) as i64 * GRID_CELL_H) as f64,
                ),
            };
            let cols = v
                .get("columns")
                .and_then(Value::as_array)
                .map(|cs| {
                    cs.iter()
                        .map(|c| Col {
                            name: s(c, "name").to_string(),
                            ty: s(c, "type").to_string(),
                            pk: c.get("isPk").and_then(Value::as_bool).unwrap_or(false),
                            fk: c.get("isFk").and_then(Value::as_bool).unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Node {
                id: s(v, "id").to_string(),
                name: s(v, "name").to_string(),
                x,
                y,
                cols,
            }
        })
        .collect();

    // fit to the content with a margin
    let min_x = nodes.iter().map(|n| n.x).fold(f64::INFINITY, f64::min);
    let min_y = nodes.iter().map(|n| n.y).fold(f64::INFINITY, f64::min);
    let max_x = nodes
        .iter()
        .map(|n| n.x + W)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = nodes
        .iter()
        .map(|n| n.y + n.height())
        .fold(f64::NEG_INFINITY, f64::max);
    for node in &mut nodes {
        node.x += MARGIN - min_x;
        node.y += MARGIN - min_y;
    }
    let width = (max_x - min_x + 2.0 * MARGIN).ceil() as u32;
    let height = (max_y - min_y + 2.0 * MARGIN + 16.0).ceil() as u32;

    let by_id: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">"#
    );
    let _ = write!(
        out,
        r#"<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="{}"/></marker></defs>"#,
        p.fk
    );
    let _ = write!(out, r#"<rect width="100%" height="100%" fill="{}"/>"#, p.bg);

    for e in doc
        .get("edges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let end = |k: &str| -> Option<(&Node, String)> {
            let side = e.get(k)?;
            let node = by_id.get(s(side, "table"))?;
            let col = side
                .get("columns")
                .and_then(Value::as_array)
                .and_then(|c| c.first())
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((node, col))
        };
        let (Some((src, sc)), Some((dst, dc))) = (end("from"), end("to")) else {
            continue;
        };
        // optional relationship (DBML `?`) = dashed, as on the canvas
        let optional = e
            .get("fromOptional")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || e.get("toOptional")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let _ = write!(
            out,
            r#"<path d="{}" fill="none" stroke="{}" stroke-width="1.6"{} marker-end="url(#arrow)"/>"#,
            bezier(src.x + W, src.row_y(&sc), dst.x, dst.row_y(&dc)),
            p.fk,
            if optional {
                r#" stroke-dasharray="6 4""#
            } else {
                ""
            }
        );
    }

    for node in &nodes {
        let (x, y, h) = (node.x, node.y, node.height());
        let hy = y + 1.0 + HEADER;
        let _ = write!(
            out,
            r#"<g><rect x="{}" y="{}" width="{}" height="{}" rx="10" fill="{}" stroke="{}"/>"#,
            n(x + 0.5),
            n(y + 0.5),
            n(W - 1.0),
            n(h - 1.0),
            p.card,
            p.border
        );
        let _ = write!(
            out,
            r#"<path d="M{},{} H{} V{} a9,9 0 0 0 -9,-9 H{} a9,9 0 0 0 -9,9 Z" fill="{}"/>"#,
            n(x + 1.0),
            n(hy),
            n(x + W - 1.0),
            n(y + 10.0),
            n(x + 10.0),
            p.head
        );
        let _ = write!(
            out,
            r#"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{}"/>"#,
            n(x + 1.0),
            n(hy),
            n(x + W - 1.0),
            n(hy),
            p.line
        );
        let _ = write!(
            out,
            r#"<rect x="{}" y="{}" width="9" height="9" rx="2" fill="none" stroke="{}" stroke-width="1.5"/>"#,
            n(x + 12.0),
            n(y + 13.0),
            p.accent
        );
        let ty = y + 1.0 + HEADER / 2.0;
        let _ = write!(
            out,
            r#"<text x="{}" y="{}" fill="{}" font-family="{SANS}" font-size="12.5" font-weight="650" dominant-baseline="central">{}</text>"#,
            n(x + 28.0),
            n(ty),
            p.text,
            esc(&fit(&node.name, 22))
        );
        let _ = write!(
            out,
            r#"<text x="{}" y="{}" fill="{}" font-family="{SANS}" font-size="11" text-anchor="end" dominant-baseline="central">{}c</text>"#,
            n(x + W - 12.0),
            n(ty),
            p.text3,
            node.cols.len()
        );
        let _ = write!(out, r#"<g font-family="{MONO}">"#);
        for (i, c) in node.cols.iter().enumerate() {
            let cy = y + 1.0 + HEADER + PAD_Y + i as f64 * ROW + ROW / 2.0;
            let key = if c.pk {
                Some(("PK", p.pk))
            } else if c.fk {
                Some(("FK", p.fk))
            } else {
                None
            };
            if let Some((k, color)) = key {
                let _ = write!(
                    out,
                    r#"<text x="{}" y="{}" fill="{color}" font-size="10.5" font-weight="700" text-anchor="middle" dominant-baseline="central">{k}</text>"#,
                    n(x + 20.0),
                    n(cy)
                );
            }
            let ty = fit(&c.ty, 14);
            let name = fit(&c.name, 24usize.saturating_sub(ty.chars().count()).max(6));
            let _ = write!(
                out,
                r#"<text x="{}" y="{}" fill="{}" font-size="12" dominant-baseline="central">{}</text>"#,
                n(x + 36.0),
                n(cy),
                p.text2,
                esc(&name)
            );
            let _ = write!(
                out,
                r#"<text x="{}" y="{}" fill="{}" font-size="11" text-anchor="end" dominant-baseline="central">{}</text>"#,
                n(x + W - 12.0),
                n(cy),
                p.text3,
                esc(&ty)
            );
            let _ = write!(
                out,
                r#"<circle cx="{}" cy="{}" r="3" fill="{}"/><circle cx="{}" cy="{}" r="3" fill="{}"/>"#,
                n(x),
                n(cy),
                p.fk,
                n(x + W),
                n(cy),
                p.fk
            );
        }
        out.push_str("</g></g>");
    }
    let _ = write!(
        out,
        r#"<text x="{}" y="{}" fill="{}" font-family="{SANS}" font-size="10" text-anchor="end" opacity="0.7">dibibest</text></svg>"#,
        width.saturating_sub(12),
        height.saturating_sub(12),
        p.text3
    );
    Some(Svg {
        svg: out,
        width,
        height,
    })
}

/// Schema source text → SVG. `from = None` detects the format. Tables are placed
/// with the grid layout, as on a fresh canvas.
pub fn render_source(
    src: &str,
    from: Option<&str>,
    opts: &RenderOptions,
) -> Result<Svg, CoreError> {
    let from = match from {
        Some(f) => f.to_string(),
        None => detect_format(src)
            .ok_or_else(|| {
                CoreError::UnknownFormat("could not detect the source format; pass `from`".into())
            })?
            .to_string(),
    };
    let conv = convert_explicit(src, &FormatId::new(from), &FormatId::new("erdjson"))?;
    let mut doc: Value = serde_json::from_str(&conv.output).map_err(|e| CoreError::Parse {
        format: "erdjson",
        message: e.to_string(),
    })?;
    crate::erdjson::apply_layout(&mut doc, crate::erdjson::LayoutStrategy::Grid)?;
    render_svg(&doc, opts).ok_or_else(|| CoreError::Parse {
        format: "erdjson",
        message: "the schema has no tables to draw".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "version": 1,
            "nodes": [
                {"id": "users", "name": "users", "position": {"x": 0, "y": 0}, "columns": [
                    {"name": "id", "type": "integer", "isPk": true},
                    {"name": "email", "type": "varchar"}]},
                {"id": "orders", "name": "orders", "position": {"x": 320, "y": 40}, "columns": [
                    {"name": "id", "type": "integer", "isPk": true},
                    {"name": "user_id", "type": "integer", "isFk": true}]},
                {"id": "odd", "name": "a<b>&\"c\"", "position": null, "columns": [
                    {"name": "x", "type": "text"}]}
            ],
            "edges": [{"id": "e1", "from": {"table": "orders", "columns": ["user_id"]},
                        "to": {"table": "users", "columns": ["id"]}, "kind": "many-to-one"}],
            "meta": {"layout": "grid"}
        })
    }

    #[test]
    fn draws_tables_columns_and_refs_fitted_to_content() {
        let mut d = doc();
        d["nodes"].as_array_mut().unwrap().pop();
        let svg = render_svg(&d, &RenderOptions::default()).unwrap();
        assert!(svg
            .svg
            .starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg""#));
        for s in [
            ">users<",
            ">orders<",
            ">email<",
            ">user_id<",
            ">PK<",
            ">FK<",
            ">varchar<",
        ] {
            assert!(svg.svg.contains(s), "missing {s}");
        }
        assert_eq!(svg.svg.matches("marker-end=\"url(#arrow)\"").count(), 1);
        assert_eq!(svg.width, 620); // 0..320+220 plus 40px margins
        assert!(svg.svg.ends_with("</svg>"));
    }

    #[test]
    fn escapes_names() {
        let svg = render_svg(&doc(), &RenderOptions::default()).unwrap().svg;
        assert!(svg.contains("a&lt;b&gt;&amp;&quot;c&quot;"));
        assert!(!svg.contains("a<b>"));
        assert_eq!(esc("<'&\">"), "&lt;&#39;&amp;&quot;&gt;");
    }

    #[test]
    fn hidden_tables_and_their_refs_are_left_out() {
        let hidden = HashSet::from(["users".to_string()]);
        let svg = render_svg(
            &doc(),
            &RenderOptions {
                hidden,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!svg.svg.contains(">users<"));
        assert!(!svg.svg.contains("marker-end"));
        let all = HashSet::from(["users", "orders", "odd"].map(String::from));
        assert!(render_svg(
            &doc(),
            &RenderOptions {
                hidden: all,
                ..Default::default()
            }
        )
        .is_none());
    }

    #[test]
    fn themes_and_long_names() {
        let light = RenderOptions {
            theme: Theme::Light,
            ..Default::default()
        };
        assert!(render_svg(&doc(), &light)
            .unwrap()
            .svg
            .contains("fill=\"#ffffff\""));
        assert!(render_svg(&doc(), &RenderOptions::default())
            .unwrap()
            .svg
            .contains("fill=\"#0b0d10\""));
        assert_eq!(Theme::parse("light"), Some(Theme::Light));
        assert_eq!(Theme::parse("x"), None);
        assert_eq!(fit("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn renders_from_source_with_detection() {
        let svg = render_source(
            "Table a {\n  id integer [pk]\n}\nTable b {\n  a_id integer\n}\nRef: b.a_id > a.id\n",
            None,
            &RenderOptions::default(),
        )
        .unwrap();
        assert!(svg.svg.contains(">a<") && svg.svg.contains(">a_id<"));
        assert_eq!(svg.svg.matches("marker-end").count(), 1);
        assert!(render_source("hello", None, &RenderOptions::default()).is_err());
        assert!(render_source(
            "CREATE TABLE t (id int);",
            Some("sql"),
            &RenderOptions::default()
        )
        .is_ok());
    }
}
