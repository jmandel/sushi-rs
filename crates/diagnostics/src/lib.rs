//! Diagnostics: source spans, severities, stable ordering, and SUSHI-compatible
//! message formatting. Diagnostics are data, not side effects (see port plan).

use std::cmp::Ordering;

/// Identifies a source file by stable index into the compiler's file table.
pub type FileId = u32;

/// A source location span. Matches SUSHI's line/col reporting (1-based lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: FileId,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl SourceSpan {
    pub const fn new(file: FileId, sl: u32, sc: u32, el: u32, ec: u32) -> Self {
        Self { file, start_line: sl, start_col: sc, end_line: el, end_col: ec }
    }

    /// A zero-width span at a point.
    pub const fn point(file: FileId, line: u32, col: u32) -> Self {
        Self::new(file, line, col, line, col)
    }
}

/// When a rule originates from an inserted RuleSet, SUSHI also records the
/// location where the insert rule was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedSpan {
    pub applied_file: FileId,
    pub applied: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warn",
            Severity::Error => "error",
        }
    }
}

/// Stable diagnostic codes. Per the plan we classify by code first, then match
/// text. Extend as parity work surfaces new SUSHI messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    DuplicateName,
    RequiredMetadata,
    ParseError,
    LexError,
    UnknownRuleSet,
    CircularInsert,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub message: String,
    pub source: Option<SourceSpan>,
    /// Monotonic sequence assigned at the same logical point SUSHI emits, used
    /// for stable ordering across an otherwise unordered collection.
    pub order: u64,
}

impl Diagnostic {
    pub fn error(code: DiagnosticCode, message: impl Into<String>, source: Option<SourceSpan>) -> Self {
        Self { severity: Severity::Error, code, message: message.into(), source, order: 0 }
    }
    pub fn warning(code: DiagnosticCode, message: impl Into<String>, source: Option<SourceSpan>) -> Self {
        Self { severity: Severity::Warning, code, message: message.into(), source, order: 0 }
    }
}

/// Collects diagnostics and hands out monotonic order values so emission order
/// is deterministic regardless of intermediate data structures.
#[derive(Debug, Default)]
pub struct DiagnosticSink {
    items: Vec<Diagnostic>,
    next_order: u64,
}

impl DiagnosticSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, mut d: Diagnostic) {
        d.order = self.next_order;
        self.next_order += 1;
        self.items.push(d);
    }

    pub fn error(&mut self, code: DiagnosticCode, message: impl Into<String>, source: Option<SourceSpan>) {
        self.push(Diagnostic::error(code, message, source));
    }

    pub fn warning(&mut self, code: DiagnosticCode, message: impl Into<String>, source: Option<SourceSpan>) {
        self.push(Diagnostic::warning(code, message, source));
    }

    pub fn errors(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Error).count()
    }

    pub fn warnings(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Warning).count()
    }

    /// Returns diagnostics in stable emission order (by `order`).
    pub fn sorted(&self) -> Vec<&Diagnostic> {
        let mut v: Vec<&Diagnostic> = self.items.iter().collect();
        v.sort_by(cmp_order);
        v
    }

    pub fn into_inner(self) -> Vec<Diagnostic> {
        self.items
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.items.iter()
    }
}

fn cmp_order(a: &&Diagnostic, b: &&Diagnostic) -> Ordering {
    a.order.cmp(&b.order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_order_assigned() {
        let mut sink = DiagnosticSink::new();
        sink.error(DiagnosticCode::ParseError, "first", None);
        sink.warning(DiagnosticCode::DuplicateName, "second", None);
        let s = sink.sorted();
        assert_eq!(s[0].message, "first");
        assert_eq!(s[1].message, "second");
        assert_eq!(s[0].order, 0);
        assert_eq!(s[1].order, 1);
        assert_eq!(sink.errors(), 1);
        assert_eq!(sink.warnings(), 1);
    }
}
