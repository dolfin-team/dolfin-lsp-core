//! `textDocument/references` provider.
//!
//! Split into two functions so the caller can release the document lock before
//! scanning all workspace ASTs (avoids holding `world.documents` while calling
//! `world.all_file_asts()`).

use lsp_types::{Location, Position, Url};

use dolfin_analysis::references::find_references_in_file;

use crate::{
    diagnostics::{offset_of, span_to_range},
    world::Document,
};

/// Identify the canonical symbol name under the cursor.
/// Returns `None` if the cursor is not on a known symbol.
pub fn target_at(doc: &Document, pos: Position) -> Option<String> {
    let analysis = doc.analysis.as_ref()?;
    let byte_offset = offset_of(&doc.rope, pos);
    let source = doc.rope.to_string();
    let word = word_at(&source, byte_offset);
    if word.is_empty() {
        return None;
    }
    let sym = analysis.index.get(word)?;
    Some(sym.name.clone())
}

/// Search `all_asts` for every reference to `target` and return LSP locations.
pub fn search(
    all_asts: &[(String, rowl::OntologyFile)],
    target: &str,
) -> Vec<Location> {
    let mut locations = Vec::new();
    for (file_uri_str, file_ast) in all_asts {
        let spans = find_references_in_file(file_ast, target);
        if spans.is_empty() {
            continue;
        }
        let Ok(file_url) = Url::parse(file_uri_str) else {
            continue;
        };
        for span in spans {
            locations.push(Location {
                uri: file_url.clone(),
                range: span_to_range(span),
            });
        }
    }
    locations
}

pub fn word_at(source: &str, byte_offset: usize) -> &str {
    let bytes = source.as_bytes();
    let start = (0..byte_offset)
        .rev()
        .find(|&i| !is_ident(bytes[i] as char))
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = (byte_offset..source.len())
        .find(|&i| !is_ident(bytes[i] as char))
        .unwrap_or(source.len());
    &source[start..end]
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
