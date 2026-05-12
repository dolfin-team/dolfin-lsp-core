//! Hover provider — builds rich markdown for the symbol under the cursor.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position, Range};
use rowl::comment::CommentMap;
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

    let (md, range) = hover_in_file(file, &analysis.index, &doc.comment_map, loc)?;
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
    comment_map: &CommentMap,
    pos: Location,
) -> Option<(String, Option<Range>)> {
    for decl in &file.declarations {
        if let Some(r) = hover_decl(decl, index, comment_map, pos) {
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
    comment_map: &CommentMap,
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
            let md = if let Some(span) = c.span {
                let desc = description_from_leading(comment_map.leading_comments(&span));
                desc.unwrap_or_else(|| concept_hover(c))
            } else {
                concept_hover(c)
            };
            Some((md, c.span.map(span_to_range)))
        }

        Declaration::Property(p) => {
            if !contains(p.span, pos) {
                return None;
            }
            hover_type_ref(&p.domain, index, pos)
                .or_else(|| hover_type_ref(&p.range, index, pos))
                .or_else(|| {
                    let md = if let Some(span) = p.span {
                        description_from_leading(comment_map.leading_comments(&span))
                    } else {
                        None
                    };
                    let md = md.unwrap_or_else(|| {
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
                        format!(
                            "```dolfin\nproperty {}: {}{} → {}{}\n```\n\nRelation `{}` → `{}`.",
                            p.name.get(),
                            dc,
                            p.domain,
                            rc,
                            p.range,
                            p.domain,
                            p.range
                        )
                    });
                    Some((md, p.span.map(span_to_range)))
                })
        }

        Declaration::Rule(r) => {
            if !contains(r.span, pos) {
                return None;
            }
            let md = r.span
                .and_then(|span| description_from_leading(comment_map.leading_comments(&span)))
                .unwrap_or_else(|| format!(
                    "```dolfin\nrule {}\n```\n\n{} pattern(s), {} assertion(s).",
                    r.name,
                    r.match_block.patterns.len(),
                    r.then_block.items.len()
                ));
            Some((md, r.span.map(span_to_range)))
        }

        Declaration::Fact(f) => {
            if !contains(f.span, pos) {
                return None;
            }
            let md = f.span
                .and_then(|span| description_from_leading(comment_map.leading_comments(&span)))
                .unwrap_or_else(|| format!(
                    "```dolfin\nfact {} a {}\n```",
                    f.id,
                    f.types.iter().map(|t| t.full()).collect::<Vec<_>>().join(", ")
                ));
            Some((md, f.span.map(span_to_range)))
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

fn description_from_leading(comments: &[rowl::comment::Comment]) -> Option<String> {
    let lines: Vec<&str> = comments
        .iter()
        .flat_map(|c| c.text.lines())
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('@'))
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

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
