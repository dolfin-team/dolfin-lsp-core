//! Document symbols provider.
//!
//! Implements `textDocument/documentSymbols`, which powers the breadcrumb
//! navigation and outline panel in editors.

use lsp_types::{DocumentSymbol, Range, SymbolKind};
use rowl::{Declaration, OntologyFile, error::Span};

use crate::diagnostics::span_to_range;
use crate::world::Document;

/// Build a hierarchical symbol tree for the outline panel.
pub fn provide(doc: &Document) -> Vec<DocumentSymbol> {
    let Some(file) = doc.parse.ontology.as_ref() else {
        return vec![];
    };
    document_symbols(file)
}

fn document_symbols(file: &OntologyFile) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for decl in &file.declarations {
        if let Some(sym) = decl_to_symbol(decl) {
            out.push(sym);
        }
    }
    out
}

fn decl_to_symbol(decl: &Declaration) -> Option<DocumentSymbol> {
    match decl {
        Declaration::Concept(c) => {
            let range = fallback_range(c.span);
            let children: Vec<DocumentSymbol> = c
                .has_declarations
                .iter()
                .map(|has| {
                    let r = fallback_range(has.span);
                    #[allow(deprecated)]
                    DocumentSymbol {
                        name: has.name.clone(),
                        detail: Some(has.type_ref.to_string()),
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range: r,
                        selection_range: r,
                        children: None,
                    }
                })
                .collect();
            let parents: Vec<_> = c.parents.iter().map(|p| p.to_string()).collect();
            let detail = if parents.is_empty() {
                None
            } else {
                Some(format!("sub {}", parents.join(", ")))
            };
            #[allow(deprecated)]
            Some(DocumentSymbol {
                name: c.name.get().clone(),
                detail,
                kind: SymbolKind::CLASS,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: Some(children),
            })
        }

        Declaration::Property(p) => {
            let range = fallback_range(p.span);
            #[allow(deprecated)]
            Some(DocumentSymbol {
                name: p.name.get().clone(),
                detail: Some(format!("{} → {}", p.domain, p.range)),
                kind: SymbolKind::FUNCTION,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            })
        }

        Declaration::Fact(_) => None,

        Declaration::Rule(r) => {
            let range = fallback_range(r.span);
            #[allow(deprecated)]
            Some(DocumentSymbol {
                name: r.name.clone(),
                detail: Some(format!(
                    "{} pattern(s), {} assertion(s)",
                    r.match_block.patterns.len(),
                    r.then_block.items.len(),
                )),
                kind: SymbolKind::NAMESPACE,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            })
        }
    }
}

fn fallback_range(span: Option<Span>) -> Range {
    span.map(span_to_range).unwrap_or_default()
}
