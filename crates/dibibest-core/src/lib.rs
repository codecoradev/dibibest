//! Dibibest core IR — the hub every format converts through.
//!
//! Design (hybrid registry): format codecs implement [`Codec`]; callers use the
//! thin convenience layer ([`convert`], [`convert_explicit`]). Fidelity reporting
//! is first-class: every conversion reports what survived ([`Fidelity`]).

pub mod dbml;
pub mod erdjson;
pub mod ir;
pub mod prisma;
pub mod registry;
pub mod scan;
pub mod sql;

pub use ir::{Column, IndexDef, Ref, RefKind, SchemaIR, Table};
pub use registry::{
    convert_explicit, default_registry, detect_format, registry_convert, Conversion, Fidelity,
    FormatId, Outcome, Registry,
};
pub use scan::SourceCounts;

/// Every fact a caller must know to use a codec: its id, its feature surface, and
/// the two directions it serves. This is the seam format plugins plug into.
pub trait Codec {
    /// Stable registry id, e.g. `FormatId("dbml")`.
    fn id(&self) -> &str;
    /// What this codec can honestly do — drives fidelity reporting.
    fn caps(&self) -> Caps;
    fn parse(&self, src: &str) -> Result<SchemaIR, CoreError>;
    fn emit(&self, ir: &SchemaIR) -> Result<String, CoreError>;
    /// Parser-independent entity counts declared by the SOURCE text (see
    /// [`scan`]). `None` = this codec has no scan; fidelity falls back to the IR.
    fn scan(&self, _src: &str) -> Option<SourceCounts> {
        None
    }
}

/// Honest feature surface of a codec. A capability listed as `false` means
/// structured data WILL be dropped on emit — and the fidelity report says so.
#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub indexes: bool,
    /// Expression / partial indexes (columns empty, `IndexDef::expr` set).
    pub expr_indexes: bool,
    pub enums: bool,
    pub defaults: bool,
    pub notes: bool,
    pub dialects: &'static [&'static str],
}

/// Every variant maps to a stable machine-readable code via [`CoreError::code`];
/// the full catalog lives in `docs/errors.md`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("parse error in {format}: {message}")]
    Parse {
        format: &'static str,
        message: String,
    },
    #[error("emit error for {format}: {message}")]
    Emit {
        format: &'static str,
        message: String,
    },
    #[error("unknown format: {0}")]
    UnknownFormat(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl CoreError {
    /// Stable error code (see `docs/errors.md`). Codes never change meaning.
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::Parse {
                format: "dbml",
                message,
            } => dbml::error_code(message),
            CoreError::Parse { .. } => "parse-error",
            CoreError::Emit { .. } => "emit-error",
            // ponytail: keyed on the message prefix; give detection its own variant if more callers need it
            CoreError::UnknownFormat(m) if m.starts_with("could not detect") => "detect-failed",
            CoreError::UnknownFormat(_) => "unknown-format",
            CoreError::Internal(_) => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dbml_code(src: &str) -> &'static str {
        dbml::DbmlCodec.parse(src).unwrap_err().code()
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(dbml_code("Table a {\n id int [pk]\n"), "unclosed-block");
        assert_eq!(dbml_code("}"), "unbalanced-brace");
        assert_eq!(dbml_code("Foo bar"), "unrecognized-statement");
        assert_eq!(dbml_code("Table a {\n !!!\n}"), "bad-column-line");
        assert_eq!(
            dbml_code("Table a {\n id int\n indexes {\n (a\n }\n}"),
            "bad-index-line"
        );
        assert_eq!(dbml_code("Ref: nonsense"), "bad-ref-line");
        assert_eq!(
            sql::SqlCodec::default()
                .parse("CREATE TABL")
                .unwrap_err()
                .code(),
            "parse-error"
        );
        assert_eq!(
            CoreError::UnknownFormat("x".into()).code(),
            "unknown-format"
        );
        assert_eq!(
            registry_convert("???", &FormatId::new("sql"))
                .unwrap_err()
                .code(),
            "detect-failed"
        );
        let emit = CoreError::Emit {
            format: "x",
            message: String::new(),
        };
        assert_eq!(emit.code(), "emit-error");
        assert_eq!(CoreError::Internal(String::new()).code(), "internal");
    }
}
