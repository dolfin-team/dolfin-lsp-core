//! `textDocument/codeLens` provider.
//!
//! Shows an inline "N references" annotation above every concept, property,
//! and rule declaration.

use lsp_types::{CodeLens, Command, Position, Range, Url};

use dolfin_analysis::references::find_references_in_file;

use crate::{
    diagnostics::span_to_range,
    world::Document,
};

/// Compute CodeLens entries for `doc` at `uri`.
///
/// `all_asts` must be obtained via `World::all_file_asts()` *before* the
/// caller acquires the document lock, to avoid deadlock.
pub fn provide(
    all_asts: &[(String, rowl::OntologyFile)],
    uri: &Url,
    doc: &Document,
) -> Vec<CodeLens> {
    let file = match &doc.parse.ontology {
        Some(f) => f,
        None => return vec![],
    };

    let mut lenses = Vec::new();

    for decl in &file.declarations {
        let (name, def_span) = match decl {
            rowl::Declaration::Concept(c) => (c.name.get(), c.span),
            rowl::Declaration::Property(p) => (p.name.get(), p.span),
            rowl::Declaration::Rule(r) => (&r.name, r.span),
            rowl::Declaration::Fact(_) => continue,
        };

        let def_span = match def_span {
            Some(s) => s,
            None => continue,
        };

        let mut ref_locations: Vec<serde_json::Value> = Vec::new();
        for (file_uri_str, file_ast) in all_asts {
            let spans = find_references_in_file(file_ast, name);
            let Ok(file_url) = Url::parse(file_uri_str) else {
                continue;
            };
            for span in spans {
                let range = span_to_range(span);
                ref_locations.push(serde_json::json!({
                    "uri": file_url.as_str(),
                    "range": {
                        "start": { "line": range.start.line, "character": range.start.character },
                        "end":   { "line": range.end.line,   "character": range.end.character },
                    }
                }));
            }
        }

        let count = ref_locations.len();
        let label = if count == 1 {
            "1 reference".to_string()
        } else {
            format!("{count} references")
        };

        let decl_range = span_to_range(def_span);
        let lens_range = Range {
            start: Position {
                line: decl_range.start.line,
                character: 0,
            },
            end: Position {
                line: decl_range.start.line,
                character: 0,
            },
        };

        let command = Command {
            title: label,
            command: "editor.action.showReferences".to_string(),
            arguments: Some(vec![
                serde_json::json!(uri.as_str()),
                serde_json::json!({
                    "line": decl_range.start.line,
                    "character": decl_range.start.character,
                }),
                serde_json::Value::Array(ref_locations),
            ]),
        };

        lenses.push(CodeLens {
            range: lens_range,
            command: Some(command),
            data: None,
        });
    }

    lenses
}
