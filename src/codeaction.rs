//! `textDocument/codeAction` — surfaces lint `FixSuggestion`s as Quick Fix actions.
//!
//! For every lint diagnostic that (a) overlaps the requested range and (b)
//! carries a [`dolfin_diagnostic::FixSuggestion`], this module emits one
//! [`lsp_types::CodeAction`] with the edits embedded directly.  No
//! `codeAction/resolve` round-trip is needed.

use std::collections::HashMap;

use dolfin_diagnostic::lsp as ddiag_lsp;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, TextEdit, Url, WorkspaceEdit,
};

use crate::diagnostics::to_lsp;
use crate::world::Document;

/// Return all applicable Quick Fix code actions for the given document range.
///
/// The `range` is typically the cursor position or the highlighted squiggle.
/// Every lint diagnostic whose span overlaps `range` and that has a fix will
/// produce one `CodeAction`.
pub fn provide(
    doc: &Document,
    uri: &Url,
    range: lsp_types::Range,
) -> Vec<CodeActionOrCommand> {
    let mut actions: Vec<CodeActionOrCommand> = Vec::new();

    for lint_diag in &doc.lint {
        let fix = match &lint_diag.fix {
            Some(f) => f,
            None => continue,
        };

        // Compute the LSP range for this diagnostic's primary span.
        let diag_range = lint_diag
            .span
            .map(ddiag_lsp::span_to_range)
            .unwrap_or_default();

        // Skip if this diagnostic doesn't touch the requested range.
        if !ranges_overlap(diag_range, range) {
            continue;
        }

        // Group edits by target file.  An empty `file` means the same file as
        // the diagnostic (i.e. the current document).
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for e in &fix.edits {
            let target_uri = if e.file.is_empty() {
                uri.clone()
            } else {
                match Url::parse(&e.file) {
                    Ok(u) => u,
                    Err(_) => uri.clone(), // fallback: treat as same-file
                }
            };
            changes.entry(target_uri).or_default().push(TextEdit {
                range: ddiag_lsp::span_to_range(e.span),
                new_text: e.replacement.clone(),
            });
        }

        if changes.is_empty() {
            continue;
        }

        let action = CodeAction {
            title: fix.description.clone(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![to_lsp(lint_diag)]),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            is_preferred: Some(true),
            ..Default::default()
        };

        actions.push(CodeActionOrCommand::CodeAction(action));
    }

    actions
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn position_le(a: lsp_types::Position, b: lsp_types::Position) -> bool {
    a.line < b.line || (a.line == b.line && a.character <= b.character)
}

/// Returns `true` if two LSP ranges share at least one position.
fn ranges_overlap(a: lsp_types::Range, b: lsp_types::Range) -> bool {
    position_le(a.start, b.end) && position_le(b.start, a.end)
}
