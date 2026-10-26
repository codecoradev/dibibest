//! Registry + thin public API. This is the entire public surface callers learn:
//! `convert` (auto-detect), `convert_explicit`, and the `Conversion` result with
//! first-class [`Fidelity`]. Formats are plugins: implement [`Codec`], register.

use crate::sql::{Dialect, SqlCodec};
use crate::{Caps, Codec, CoreError, SchemaIR, SourceCounts};
use serde::{Deserialize, Serialize};

/// Convenience wrapper so callers can pass `"dbml"`, `Format::Dbml`, or their
/// own ids. Registry ids are plain strings on purpose: adding a format must
/// never require changing this crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatId(pub String);

impl FormatId {
    pub fn new(s: impl Into<String>) -> Self {
        FormatId(s.into())
    }
}

impl std::fmt::Display for FormatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversion {
    pub output: String,
    pub fidelity: Fidelity,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Fidelity {
    pub tables: Outcome,
    pub columns: Outcome,
    pub refs: Outcome,
    pub indexes: Outcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
    /// true when parse(emit(ir)) == ir for the SOURCE format (lossless round-trip)
    pub idempotent: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub kept: usize,
    pub total: usize,
}

impl Outcome {
    fn of(kept: usize, total: usize) -> Self {
        Outcome { kept, total }
    }
    pub fn lossless(&self) -> bool {
        self.kept == self.total
    }
}

pub struct Registry {
    codecs: Vec<Box<dyn Codec>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl Registry {
    /// Built-in codecs: dbml, sql (PostgreSQL), sql:mysql, sql:sqlite, prisma, erdjson.
    pub fn with_defaults() -> Self {
        Registry {
            codecs: vec![
                Box::new(crate::dbml::DbmlCodec),
                Box::new(SqlCodec::new(Dialect::Postgres)),
                Box::new(SqlCodec::new(Dialect::MySql)),
                Box::new(SqlCodec::new(Dialect::Sqlite)),
                Box::new(crate::prisma::PrismaCodec),
                Box::new(crate::erdjson::ErdJsonCodec),
            ],
        }
    }

    pub fn register(&mut self, codec: Box<dyn Codec>) {
        self.codecs.push(codec);
    }

    pub fn ids(&self) -> Vec<String> {
        self.codecs.iter().map(|c| c.id().to_string()).collect()
    }

    /// Public: tests and callers that need raw codec access at the seam.
    pub fn codec(&self, id: &FormatId) -> Result<&dyn Codec, CoreError> {
        self.codecs
            .iter()
            .find(|c| c.id() == id.0)
            .map(|c| c.as_ref())
            .ok_or_else(|| CoreError::UnknownFormat(id.0.clone()))
    }

    fn parse_with(&self, src: &str, from: &FormatId) -> Result<SchemaIR, CoreError> {
        self.codec(from)?.parse(src)
    }

    /// Parse source text into the IR (public: web/canvas and tests need this).
    pub fn parse(&self, src: &str, from: &FormatId) -> Result<SchemaIR, CoreError> {
        self.parse_with(src, from)
    }

    /// Emit IR into a target format (public: web/canvas and tests need this).
    pub fn emit(&self, ir: &SchemaIR, to: &FormatId) -> Result<String, CoreError> {
        self.codec(to)?.emit(ir)
    }

    pub fn convert_explicit(
        &self,
        src: &str,
        from: &FormatId,
        to: &FormatId,
    ) -> Result<Conversion, CoreError> {
        let ir = self.parse_with(src, from)?;
        self.measure(src, from, to, ir)
    }

    /// Emit `ir` into `to` and measure fidelity against the SOURCE TEXT: expected
    /// totals come from the source codec's [`Codec::scan`] (parser-independent),
    /// kept counts from re-parsing the emitted output. Anything the scan finds
    /// that `ir` lacks is parse-time loss: reported in `dropped` and it clears
    /// `idempotent`. `ir` is a parameter so tests can hand in a doctored IR.
    pub fn measure(
        &self,
        src: &str,
        from: &FormatId,
        to: &FormatId,
        ir: SchemaIR,
    ) -> Result<Conversion, CoreError> {
        let to_codec = self.codec(to)?;
        let output = to_codec.emit(&ir)?;
        let reparsed = to_codec.parse(&output)?;

        let ir_counts = SourceCounts {
            tables: ir.tables.len(),
            columns: ir.tables.iter().map(|t| t.columns.len()).sum(),
            refs: ir.refs.len(),
            indexes: ir.tables.iter().map(|t| t.indexes.len()).sum(),
        };
        let kept = SourceCounts {
            tables: reparsed.tables.len(),
            columns: reparsed.tables.iter().map(|t| t.columns.len()).sum(),
            refs: reparsed.refs.len(),
            indexes: reparsed.tables.iter().map(|t| t.indexes.len()).sum(),
        };
        // A scan can under-count (never over-count): trust whichever is larger.
        let scanned = self.codec(from)?.scan(src).unwrap_or_default();
        let expect = SourceCounts {
            tables: scanned.tables.max(ir_counts.tables),
            columns: scanned.columns.max(ir_counts.columns),
            refs: scanned.refs.max(ir_counts.refs),
            indexes: scanned.indexes.max(ir_counts.indexes),
        };

        let mut dropped = report_caps_gaps(&ir, from, to_codec);
        let mut parse_loss = false;
        for (label, want, have) in [
            ("table", expect.tables, ir_counts.tables),
            ("column", expect.columns, ir_counts.columns),
            ("ref", expect.refs, ir_counts.refs),
            ("index", expect.indexes, ir_counts.indexes),
        ] {
            if want > have {
                parse_loss = true;
                dropped.push(format!(
                    "{} {label}(s) declared in source but missing after parse — parser loss",
                    want - have
                ));
            }
        }
        let fidelity = Fidelity {
            tables: Outcome::of(kept.tables, expect.tables),
            columns: Outcome::of(kept.columns, expect.columns),
            refs: Outcome::of(kept.refs, expect.refs),
            indexes: Outcome::of(kept.indexes, expect.indexes),
            dropped,
            idempotent: !parse_loss && self.check_idempotent(from, &ir),
        };
        Ok(Conversion { output, fidelity })
    }

    /// One-liner with format auto-detection (extension- and shape-based).
    pub fn convert(&self, src: &str, to: &FormatId) -> Result<Conversion, CoreError> {
        let from = detect_format(src)
            .ok_or_else(|| CoreError::UnknownFormat("could not detect source format".into()))?;
        self.convert_explicit(src, &FormatId::new(from), to)
    }

    /// Idempotency: parse(emit(parse(src))) in the SOURCE format must equal the
    /// first IR. This is the "bolak-balik" promise, measured on every conversion.
    fn check_idempotent(&self, from: &FormatId, first_ir: &SchemaIR) -> bool {
        let codec = match self.codec(from) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let emitted = match codec.emit(first_ir) {
            Ok(e) => e,
            Err(_) => return false,
        };
        match codec.parse(&emitted) {
            Ok(second) => second == *first_ir,
            Err(_) => false,
        }
    }
}

fn report_caps_gaps(ir: &SchemaIR, _from: &FormatId, to: &dyn Codec) -> Vec<String> {
    let mut gaps = Vec::new();
    let caps: Caps = to.caps();
    let has_indexes = ir.tables.iter().any(|t| !t.indexes.is_empty());
    let has_enums = !ir.enums.is_empty();
    let has_notes = ir
        .tables
        .iter()
        .any(|t| t.note.is_some() || t.columns.iter().any(|c| c.note.is_some()));
    if has_indexes && !caps.indexes {
        gaps.push("indexes have no equivalent in target format".into());
    }
    let expr: Vec<String> =
        ir.tables
            .iter()
            .flat_map(|t| {
                t.indexes.iter().filter(|i| i.expr.is_some()).map(move |i| {
                    format!("{}.{}", t.name, i.name.as_deref().unwrap_or("<unnamed>"))
                })
            })
            .collect();
    if !expr.is_empty() && !caps.expr_indexes {
        gaps.push(format!(
            "{} expression/partial index(es) dropped — no equivalent in target format: {}",
            expr.len(),
            expr.join(", ")
        ));
    }
    if has_enums && !caps.enums {
        gaps.push("enums have no equivalent in target format".into());
    }
    if has_notes && !caps.notes {
        gaps.push("notes/comments have no equivalent in target format".into());
    }
    gaps
}

/// Heuristic format detection — mirrors Liam/dbdiagram's file-name-first
/// behavior, but content-based so pasted text works too.
pub fn detect_format(src: &str) -> Option<&'static str> {
    let t = src.trim_start();
    if t.starts_with('{') && t.contains("\"nodes\"") && t.contains("\"edges\"") {
        return Some("erdjson");
    }
    let upper_head: String = t
        .chars()
        .take(4096)
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    let head = upper_head.to_ascii_uppercase();
    if t.starts_with("Project ")
        || t.starts_with("Table ")
        || t.starts_with("Ref")
        || t.starts_with("Enum ")
        || t.contains("Ref: ")
    {
        return Some("dbml");
    }
    if head.contains("CREATE TABLE")
        || head.contains("CREATE INDEX")
        || head.contains("CREATE UNIQUE INDEX")
    {
        return Some("sql");
    }
    if t.contains("model ") && t.contains('{') && t.contains("datasource")
        || t.starts_with("// Prisma")
    {
        return Some("prisma");
    }
    None
}

/// Free function so callers don't even need a Registry for the default set.
pub fn default_registry() -> Registry {
    Registry::with_defaults()
}

/// Free one-liner: convert with auto-detected source format (default registry).
pub fn registry_convert(src: &str, to: &FormatId) -> Result<Conversion, CoreError> {
    Registry::with_defaults().convert(src, to)
}

/// Free variant with explicit source format (default registry).
pub fn convert_explicit(
    src: &str,
    from: &FormatId,
    to: &FormatId,
) -> Result<Conversion, CoreError> {
    Registry::with_defaults().convert_explicit(src, from, to)
}
