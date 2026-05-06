//! Diagnostic conversion + position/span utilities.
//!
//! Position helpers use [`ropey::Rope`] for O(log n) line/char lookups
//! instead of iterating the raw string.

use dolfin_diagnostic::lsp as ddiag_lsp;
use lsp_types::{self, Range};
use ropey::Rope;
use rowl::error::{Location, Span};

use crate::world::Document;

// ── Diagnostic collection ─────────────────────────────────────────────────────

/// Collect all parse + analysis + lint diagnostics for a document as LSP diagnostics.
pub fn collect(doc: &Document) -> Vec<lsp_types::Diagnostic> {
    let mut out = Vec::new();

    for d in doc.parse.unified_diagnostics() {
        out.push(to_lsp(&d));
    }

    let zero = Range::default();
    if let Some(a) = &doc.analysis {
        for d in &a.diagnostics {
            let mut lsp_diag = to_lsp(d);
            if lsp_diag.range == Range::default() {
                lsp_diag.range = zero;
            }
            out.push(lsp_diag);
        }
    }

    for d in &doc.lint {
        out.push(to_lsp(d));
    }

    out
}

/// Convert a unified `dolfin_diagnostic::Diagnostic` to an LSP diagnostic.
pub fn to_lsp(d: &dolfin_diagnostic::Diagnostic) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range: d.span.map(ddiag_lsp::span_to_range).unwrap_or_default(),
        severity: Some(d.severity.into()),
        code: Some(lsp_types::NumberOrString::String(d.code.code_str())),
        source: Some("dolfin".into()),
        message: d.message.clone(),
        ..Default::default()
    }
}

// ── Position / span utilities ─────────────────────────────────────────────────

/// Convert an LSP `Position` (0-based line/char) to a byte offset.
pub fn offset_of(rope: &Rope, pos: lsp_types::Position) -> usize {
    let line = pos.line as usize;
    let col = pos.character as usize;
    if line >= rope.len_lines() {
        return rope.len_bytes();
    }
    let line_start_char = rope.line_to_char(line);
    let char_idx = (line_start_char + col).min(rope.len_chars());
    rope.char_to_byte(char_idx)
}

/// rowl `Span` → LSP `Range`.
pub fn span_to_range(span: Span) -> lsp_types::Range {
    ddiag_lsp::span_to_range(span.into())
}

/// rowl `Location` (1-based) → LSP `Position` (0-based).
pub fn location_to_position(loc: Location) -> lsp_types::Position {
    ddiag_lsp::location_to_position(loc.into())
}

/// LSP `Position` (0-based) → rowl `Location` (offset = 0; fill with [`offset_of`]).
pub fn position_to_location(pos: lsp_types::Position) -> Location {
    ddiag_lsp::position_to_location(pos).into()
}

/// Returns `true` if `span` contains `byte_offset`.
pub fn span_contains(span: Option<Span>, byte_offset: usize) -> bool {
    ddiag_lsp::span_contains(span.map(Into::into), byte_offset)
}
