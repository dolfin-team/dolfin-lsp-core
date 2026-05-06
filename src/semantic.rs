//! LSP Semantic Tokens provider.
//!
//! Scans the source with the `rowl` lexer for exact token positions and
//! classifies each token using the `SymbolIndex` built by `dolfin-analysis`.
//!
//! # Token type legend
//! Index | LSP name      | Used for
//! 0     | namespace     | prefix aliases
//! 1     | type          | type references (concept/enum name in a type position)
//! 2     | class         | concept declaration names
//! 3     | enum          | enum declaration names
//! 4     | enumMember    | enum variant names
//! 5     | function      | property declaration names + `has` property names
//! 6     | macro         | rule declaration names
//! 7     | variable      | ?variables inside rules
//! 8     | keyword       | language keywords
//!
//! # Token modifier legend
//! Bit 0 | declaration   | token is at the definition site of a symbol

use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensLegend,
};
use rowl::lexer::{Lexer, Token};

use dolfin_analysis::SymbolIndex;

use crate::world::Document;

// ── Legend ────────────────────────────────────────────────────────────────────

pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE,   // 0
    SemanticTokenType::TYPE,        // 1
    SemanticTokenType::CLASS,       // 2
    SemanticTokenType::ENUM,        // 3
    SemanticTokenType::ENUM_MEMBER, // 4
    SemanticTokenType::FUNCTION,    // 5
    SemanticTokenType::MACRO,       // 6
    SemanticTokenType::VARIABLE,    // 7
    SemanticTokenType::KEYWORD,     // 8
];

pub const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION, // bit 0
];

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

// ── Token type index constants ────────────────────────────────────────────────

const TT_NAMESPACE: u32 = 0;
const TT_TYPE: u32 = 1;
const TT_CLASS: u32 = 2;
const TT_ENUM_MEMBER: u32 = 4;
const TT_FUNCTION: u32 = 5;
const TT_MACRO: u32 = 6;
const TT_VARIABLE: u32 = 7;
const TT_KEYWORD: u32 = 8;

const MOD_DECLARATION: u32 = 1 << 0;

// ── Raw (pre-delta) token entry ───────────────────────────────────────────────

struct RawToken {
    line: u32,
    col: u32,
    len: u32,
    token_type: u32,
    modifiers: u32,
}

// ── Context state machine ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    None,
    AfterConcept,
    AfterProperty,
    AfterRule,
    AfterPrefix,
    AfterHas,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Compute delta-encoded semantic tokens for the full document.
pub fn full_tokens(doc: &Document) -> Option<SemanticTokens> {
    let index = doc.analysis.as_ref().map(|a| &a.index);
    let mut raw: Vec<RawToken> = Vec::new();
    let mut ctx = Context::None;

    let source = doc.rope.to_string();
    let lexer = Lexer::new(&source);
    for result in lexer {
        let Ok((start, token, end)) = result else {
            ctx = Context::None;
            continue;
        };

        let line = start.line.saturating_sub(1) as u32;
        let col = start.column.saturating_sub(1) as u32;
        let len = end.offset.saturating_sub(start.offset) as u32;

        match &token {
            // ── Declaration keywords ──────────────────────────────────────
            Token::Concept => {
                push_kw(&mut raw, line, col, len);
                ctx = Context::AfterConcept;
            }
            Token::Property => {
                push_kw(&mut raw, line, col, len);
                ctx = Context::AfterProperty;
            }
            Token::Rule => {
                push_kw(&mut raw, line, col, len);
                ctx = Context::AfterRule;
            }
            Token::Prefix => {
                push_kw(&mut raw, line, col, len);
                ctx = Context::AfterPrefix;
            }
            Token::Has => {
                push_kw(&mut raw, line, col, len);
                ctx = Context::AfterHas;
            }

            // ── Other keywords ────────────────────────────────────────────
            Token::Package
            | Token::As
            | Token::IriName
            | Token::Sub
            | Token::Match
            | Token::Then
            | Token::Is
            | Token::All
            | Token::None
            | Token::AtLeast
            | Token::AtMost
            | Token::Exactly
            | Token::Between
            | Token::One
            | Token::Any
            | Token::Some
            | Token::Optional => {
                push_kw(&mut raw, line, col, len);
                ctx = Context::None;
            }

            // ── Primitive type keywords ───────────────────────────────────
            Token::TString | Token::TInt | Token::TFloat | Token::TBoolean => {
                raw.push(RawToken {
                    line,
                    col,
                    len,
                    token_type: TT_TYPE,
                    modifiers: 0,
                });
                ctx = Context::None;
            }

            // ── Identifiers ───────────────────────────────────────────────
            Token::Name(name) => {
                let (token_type, modifiers) = classify_name(name, ctx, index);
                if let Some(tt) = token_type {
                    raw.push(RawToken {
                        line,
                        col,
                        len,
                        token_type: tt,
                        modifiers,
                    });
                }
                ctx = Context::None;
            }

            // ── Rule variables ────────────────────────────────────────────
            Token::Variable(_) => {
                raw.push(RawToken {
                    line,
                    col,
                    len,
                    token_type: TT_VARIABLE,
                    modifiers: 0,
                });
                ctx = Context::None;
            }

            _ => {
                ctx = Context::None;
            }
        }
    }

    raw.sort_by_key(|t| (t.line, t.col));

    let mut tokens = Vec::with_capacity(raw.len());
    let (mut prev_line, mut prev_col) = (0u32, 0u32);
    for t in raw {
        let delta_line = t.line - prev_line;
        let delta_start = if delta_line == 0 {
            t.col - prev_col
        } else {
            t.col
        };
        tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.len,
            token_type: t.token_type,
            token_modifiers_bitset: t.modifiers,
        });
        prev_line = t.line;
        prev_col = t.col;
    }

    Some(SemanticTokens {
        result_id: None,
        data: tokens,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn push_kw(raw: &mut Vec<RawToken>, line: u32, col: u32, len: u32) {
    raw.push(RawToken {
        line,
        col,
        len,
        token_type: TT_KEYWORD,
        modifiers: 0,
    });
}

fn classify_name(name: &str, ctx: Context, index: Option<&SymbolIndex>) -> (Option<u32>, u32) {
    match ctx {
        Context::AfterConcept => (Some(TT_CLASS), MOD_DECLARATION),
        Context::AfterProperty => (Some(TT_FUNCTION), MOD_DECLARATION),
        Context::AfterRule => (Some(TT_MACRO), MOD_DECLARATION),
        Context::AfterPrefix => (Some(TT_NAMESPACE), MOD_DECLARATION),
        Context::AfterHas => (Some(TT_FUNCTION), 0),
        Context::None => {
            let tt = index.and_then(|idx| classify_by_index(name, idx));
            (tt, 0)
        }
    }
}

fn classify_by_index(name: &str, index: &SymbolIndex) -> Option<u32> {
    use dolfin_analysis::SymbolKind;
    let sym = index.get(name)?;
    Some(match &sym.kind {
        SymbolKind::Concept => TT_CLASS,
        SymbolKind::Property => TT_FUNCTION,
        SymbolKind::Individual { .. } => TT_ENUM_MEMBER,
        SymbolKind::Rule => TT_MACRO,
        SymbolKind::Prefix => TT_NAMESPACE,
    })
}
