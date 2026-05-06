//! Transport-agnostic LSP logic for the Dolfin language.
//!
//! Compiles to both native (used by the `dolfin-lsp` stdio binary via
//! `tower-lsp`) and `wasm32-unknown-unknown` (used by `dolfin-lsp-wasm` in
//! the playground Web Worker).

pub mod codeaction;
pub mod codelens;
pub mod completion;
pub mod diagnostics;
pub mod dispatch;
pub mod goto;
pub mod hover;
pub mod references;
pub mod rename;
pub mod semantic;
pub mod symbols;
pub mod world;
