//! `textDocument/prepareRename` and `textDocument/rename` — VSCode "Rename Symbol" (F2).
//!
//! `prepare` validates that there is a renameable symbol under the cursor and
//! returns its source range.  `rename` produces a full-workspace `WorkspaceEdit`
//! replacing every declaration site and reference.

use std::collections::HashMap;

use dolfin_analysis::references::find_references_in_file;
use lsp_types::{Position, PrepareRenameResponse, TextEdit, Url, WorkspaceEdit};
use ropey::Rope;
use rowl::ast::Declaration;
use rowl::error::Span;

use crate::{
    diagnostics::{offset_of, span_to_range},
    references::{target_at, word_at},
    world::{Document, World},
};

/// Validate that the cursor is on a renameable symbol.
///
/// Returns the source range of the identifier at `pos`, or `None` if the
/// cursor is not on a known symbol.
pub fn prepare(doc: &Document, pos: Position) -> Option<PrepareRenameResponse> {
    let rope = &doc.rope;
    let source = rope.to_string();
    let byte_offset = offset_of(rope, pos);
    let word = word_at(&source, byte_offset);
    if word.is_empty() {
        return None;
    }
    // Only allow renaming known symbols.
    doc.analysis.as_ref()?.index.get(word)?;

    // Compute the byte range of `word` within `source`.
    let word_start = word.as_ptr() as usize - source.as_ptr() as usize;
    let word_end = word_start + word.len();
    let start = byte_to_position(rope, word_start);
    let end = byte_to_position(rope, word_end);
    Some(PrepareRenameResponse::Range(lsp_types::Range { start, end }))
}

/// Compute a workspace-wide rename and return all required text edits.
///
/// Collects every declaration site and every reference to the symbol under
/// `pos` across all files known to the `World`.
pub fn rename(
    world: &World,
    uri: &Url,
    pos: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    // Phase 1: find the target symbol (while holding the doc lock).
    let target = world.with_doc(uri, |doc| target_at(doc, pos)).flatten()?;

    // Phase 2: collect edits from all ASTs (lock released).
    let all_asts = world.all_file_asts();
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();

    for (uri_str, ast) in &all_asts {
        let Ok(file_uri) = Url::parse(uri_str) else {
            continue;
        };

        // Declaration sites (concept / property name tokens).
        for span in declaration_spans_in_file(ast, &target) {
            changes.entry(file_uri.clone()).or_default().push(TextEdit {
                range: span_to_range(span),
                new_text: new_name.to_string(),
            });
        }

        // Reference sites (type refs, property uses in rules, etc.).
        for span in find_references_in_file(ast, &target) {
            changes.entry(file_uri.clone()).or_default().push(TextEdit {
                range: span_to_range(span),
                new_text: new_name.to_string(),
            });
        }
    }

    if changes.is_empty() {
        return None;
    }

    Some(WorkspaceEdit { changes: Some(changes), ..Default::default() })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Collect spans of declaration-site name tokens in `file` that match `target`.
fn declaration_spans_in_file(file: &rowl::OntologyFile, target: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let simple = target.split('.').next_back().unwrap_or(target);
    for decl in &file.declarations {
        match decl {
            Declaration::Concept(c) => {
                let name = c.name.get();
                if name.as_str() == target || name.as_str() == simple {
                    if let Some(s) = c.name.span {
                        out.push(s);
                    }
                }
            }
            Declaration::Property(p) => {
                let name = p.name.get();
                if name.as_str() == target || name.as_str() == simple {
                    if let Some(s) = p.name.span {
                        out.push(s);
                    }
                }
            }
            Declaration::Rule(_) => {}
            Declaration::Fact(_) => {}
        }
    }
    out
}

/// Convert a byte offset within a rope to an LSP `Position`.
fn byte_to_position(rope: &Rope, byte_offset: usize) -> Position {
    let byte_offset = byte_offset.min(rope.len_bytes());
    let char_idx = rope.byte_to_char(byte_offset);
    let line = rope.char_to_line(char_idx);
    let line_start = rope.line_to_char(line);
    let col = char_idx - line_start;
    Position {
        line: line as u32,
        character: col as u32,
    }
}
