//! Go-to-definition provider.

use lsp_types::{GotoDefinitionResponse, Location, Position, Url};

use crate::{
    diagnostics::{offset_of, span_to_range},
    world::Document,
};

/// Find the definition location of the symbol at `pos` within `doc`.
/// `uri` is used as the fallback target when the symbol has no explicit file.
pub fn provide(doc: &Document, uri: &Url, pos: Position) -> Option<GotoDefinitionResponse> {
    let analysis = doc.analysis.as_ref()?;

    let byte_offset = offset_of(&doc.rope, pos);
    let source = doc.rope.to_string();
    let word = word_at(&source, byte_offset);
    if word.is_empty() {
        return None;
    }

    let sym = analysis.index.get(word)?;
    let span = sym.definition_span?;

    let target_uri = sym
        .file
        .as_deref()
        .and_then(|f| Url::parse(f).ok())
        .unwrap_or_else(|| uri.clone());

    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: span_to_range(span),
    }))
}

fn word_at(source: &str, byte_offset: usize) -> &str {
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
