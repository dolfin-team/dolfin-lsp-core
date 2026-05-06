//! Completion provider.

use dolfin_analysis::SymbolKind;
use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse, Position};

use crate::{diagnostics::offset_of, world::Document};

static KEYWORDS: &[(&str, &str)] = &[
    ("concept", "Define a concept (class)"),
    ("property", "Define a property (relation)"),
    ("rule", "Define an inference rule"),
    ("prefix", "Declare a prefix alias"),
    ("sub", "Inherit from a parent concept"),
    ("has", "Declare a property slot"),
    ("match", "Match block of an inference rule"),
    ("then", "Then block of an inference rule"),
    ("one", "Cardinality: exactly one"),
    ("any", "Cardinality: zero or more"),
    ("some", "Cardinality: one or more"),
    ("optional", "Cardinality: zero or one"),
    ("string", "Primitive type: string"),
    ("int", "Primitive type: integer"),
    ("float", "Primitive type: floating-point"),
    ("boolean", "Primitive type: boolean"),
    ("all", "Quantifier: for all"),
    ("none", "Quantifier: for none"),
    ("at_least", "Quantifier: at least N"),
    ("at_most", "Quantifier: at most N"),
    ("exactly", "Quantifier: exactly N"),
    ("between", "Quantifier: between M and N"),
];

pub fn provide(doc: &Document, pos: Position) -> CompletionResponse {
    let byte_offset = offset_of(&doc.rope, pos);
    let source = doc.rope.to_string();
    let prefix = partial_word(&source, byte_offset);

    let mut items: Vec<CompletionItem> = Vec::new();

    for (kw, detail) in KEYWORDS {
        if kw.starts_with(prefix) {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some(detail.to_string()),
                ..Default::default()
            });
        }
    }

    if let Some(analysis) = &doc.analysis {
        for sym in analysis.index.iter() {
            if sym.name.starts_with(prefix) {
                items.push(CompletionItem {
                    label: sym.name.clone(),
                    kind: Some(symbol_kind(&sym.kind)),
                    detail: Some(sym.detail.clone()),
                    ..Default::default()
                });
            }
        }
    }

    items.sort_by(|a, b| a.label.cmp(&b.label));
    CompletionResponse::Array(items)
}

fn symbol_kind(kind: &SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::Concept => CompletionItemKind::CLASS,
        SymbolKind::Property => CompletionItemKind::FUNCTION,
        SymbolKind::Individual { .. } => CompletionItemKind::ENUM_MEMBER,
        SymbolKind::Rule => CompletionItemKind::INTERFACE,
        SymbolKind::Prefix => CompletionItemKind::MODULE,
    }
}

fn partial_word<'a>(source: &'a str, byte_offset: usize) -> &'a str {
    let bytes = source.as_bytes();
    let start = (0..byte_offset)
        .rev()
        .find(|&i| !is_ident(bytes[i] as char))
        .map(|i| i + 1)
        .unwrap_or(0);
    &source[start..byte_offset]
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
