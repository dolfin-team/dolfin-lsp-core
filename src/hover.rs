//! Hover provider — builds rich markdown for the symbol under the cursor.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position, Range};
use rowl::{Declaration, OntologyFile, TypeRef, error::Location};

use dolfin_analysis::{Symbol, SymbolIndex, SymbolKind};

use crate::{
    diagnostics::{offset_of, position_to_location, span_to_range},
    world::Document,
};

pub fn provide(doc: &Document, pos: Position) -> Option<Hover> {
    let analysis = doc.analysis.as_ref()?;
    let file = doc.parse.ontology.as_ref()?;

    let byte_offset = offset_of(&doc.rope, pos);
    let mut loc = position_to_location(pos);
    loc.offset = byte_offset;

    let (md, range) = hover_in_file(file, &analysis.index, loc)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range,
    })
}

// ── AST walker ────────────────────────────────────────────────────────────────

fn hover_in_file(
    file: &OntologyFile,
    index: &SymbolIndex,
    pos: Location,
) -> Option<(String, Option<Range>)> {
    for decl in &file.declarations {
        if let Some(r) = hover_decl(decl, index, pos) {
            return Some(r);
        }
    }
    for prefix in &file.prefixes {
        if contains(prefix.span, pos) {
            return Some((
                format!(
                    "```dolfin\nprefix {} as {}\n```\n\nAlias `{}` expands to `{}`.",
                    prefix.path, prefix.alias, prefix.alias, prefix.path,
                ),
                prefix.span.map(span_to_range),
            ));
        }
    }
    None
}

fn hover_decl(
    decl: &Declaration,
    index: &SymbolIndex,
    pos: Location,
) -> Option<(String, Option<Range>)> {
    match decl {
        Declaration::Concept(c) => {
            if !contains(c.span, pos) {
                return None;
            }
            for parent in &c.parents {
                if let Some(r) = hover_type_ref(parent, index, pos) {
                    return Some(r);
                }
            }
            for has in &c.has_declarations {
                if contains(has.span, pos) {
                    if let Some(r) = hover_type_ref(&has.type_ref, index, pos) {
                        return Some(r);
                    }
                    let card = has
                        .cardinality
                        .as_ref()
                        .map(|c| format!("{c} "))
                        .unwrap_or_default();
                    return Some((
                        format!(
                            "```dolfin\nhas {}: {}{}\n```\n\nSlot on `{}`.",
                            has.name, card, has.type_ref, c.name
                        ),
                        has.span.map(span_to_range),
                    ));
                }
            }
            Some((concept_hover(c), c.span.map(span_to_range)))
        }

        Declaration::Property(p) => {
            if !contains(p.span, pos) {
                return None;
            }
            hover_type_ref(&p.domain, index, pos)
                .or_else(|| hover_type_ref(&p.range, index, pos))
                .or_else(|| {
                    let dc = p
                        .domain_cardinality
                        .as_ref()
                        .map(|c| format!("{c} "))
                        .unwrap_or_default();
                    let rc = p
                        .range_cardinality
                        .as_ref()
                        .map(|c| format!("{c} "))
                        .unwrap_or_default();
                    Some((
                        format!(
                            "```dolfin\nproperty {}: {}{} → {}{}\n```\n\nRelation `{}` → `{}`.",
                            p.name.get(), dc, p.domain, rc, p.range, p.domain, p.range
                        ),
                        p.span.map(span_to_range),
                    ))
                })
        }

        Declaration::Rule(r) => {
            if !contains(r.span, pos) {
                return None;
            }
            Some((
                format!(
                    "```dolfin\nrule {}\n```\n\n{} pattern(s), {} assertion(s).",
                    r.name,
                    r.match_block.patterns.len(),
                    r.then_block.items.len()
                ),
                r.span.map(span_to_range),
            ))
        }
    }
}

fn hover_type_ref(
    tr: &TypeRef,
    index: &SymbolIndex,
    pos: Location,
) -> Option<(String, Option<Range>)> {
    match tr {
        TypeRef::Named { name, span } if contains(*span, pos) => {
            let range = span.map(span_to_range);
            let sym = index.get(&name.last()).or_else(|| index.get(&name.full()));
            Some(sym.map(|s| (symbol_hover(s), range)).unwrap_or_else(|| {
                (
                    format!("```dolfin\n{}\n```\n\n⚠ Unresolved.", name.full()),
                    range,
                )
            }))
        }
        TypeRef::Primitive { span, .. } if contains(*span, pos) => Some((
            format!("```dolfin\n{tr}\n```\n\nBuilt-in primitive."),
            span.map(span_to_range),
        )),
        _ => None,
    }
}

// ── Hover text builders ───────────────────────────────────────────────────────

fn concept_hover(c: &rowl::ConceptDef) -> String {
    let mut lines = vec![format!("concept {}:", c.name)];
    if !c.parents.is_empty() {
        lines.push(format!(
            "  sub {}",
            c.parents
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for has in &c.has_declarations {
        let card = has
            .cardinality
            .as_ref()
            .map(|c| format!("{c} "))
            .unwrap_or_default();
        lines.push(format!("  has {}: {}{}", has.name, card, has.type_ref));
    }
    let n = c.has_declarations.len();
    let desc = if n == 0 {
        "No properties.".into()
    } else {
        format!("{n} propert{}.", if n == 1 { "y" } else { "ies" })
    };
    format!("```dolfin\n{}\n```\n\n{desc}", lines.join("\n"))
}

fn symbol_hover(sym: &Symbol) -> String {
    if let SymbolKind::Individual { parent } = &sym.kind {
        return format!("```dolfin\n{}\n```\n\nIndividual of `{parent}`.", sym.name);
    }
    let label = match sym.kind {
        SymbolKind::Concept => "concept",
        SymbolKind::Property => "property",
        SymbolKind::Rule => "rule",
        SymbolKind::Prefix => "prefix",
        SymbolKind::Individual { .. } => unreachable!(),
    };
    format!("```dolfin\n{}\n```\n\n*{label}* `{}`", sym.detail, sym.name)
}

// ── Utility ───────────────────────────────────────────────────────────────────

fn contains(span: Option<rowl::error::Span>, pos: Location) -> bool {
    span.map_or(false, |s| {
        s.start.offset <= pos.offset && pos.offset <= s.end.offset
    })
}
