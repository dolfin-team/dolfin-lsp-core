# dolfin-lsp-core

Transport-agnostic LSP logic for the [Dolfin](https://github.com/dolfin-team) language.

This crate implements all Language Server Protocol handlers (hover, completion, diagnostics, go-to-definition, references, document symbols, code actions, rename, code lens, and semantic tokens) as pure Rust logic with no dependency on any specific transport. It compiles to both native (used by the `dolfin-lsp` stdio binary via `tower-lsp`) and `wasm32-unknown-unknown` (used by `dolfin-lsp-wasm` in the browser playground Web Worker).

## Architecture

```
dolfin-lsp-core
├── dispatch.rs     JSON-RPC dispatcher (LspState): routes requests/notifications to handlers
├── world.rs        Multi-file workspace state (World + Document)
├── completion.rs   Keyword and symbol completions
├── hover.rs        Hover documentation
├── goto.rs         Go-to-definition
├── references.rs   Find references
├── rename.rs       Prepare rename + apply rename
├── symbols.rs      Document symbols
├── codeaction.rs   Quick-fix code actions
├── codelens.rs     Code lens (cross-file usage counts)
├── semantic.rs     Semantic token highlighting
└── diagnostics.rs  Parse and lint diagnostic collection
```

### Key types

**`LspState`** (`dispatch.rs`) : top-level server state. Owns a `World` and the workspace root URI. Exposes two entry points used by both transports:

```rust
lsp_state.handle_request(id, method, params)      -> DispatchResult
lsp_state.handle_notification(method, params)     -> DispatchResult
```

`DispatchResult` carries an optional JSON response body and zero or more server-initiated `textDocument/publishDiagnostics` push notifications.

**`World`** (`world.rs`) : workspace model. Tracks every open document plus a merged `SymbolIndex` that spans both editor-open files and background files discovered on disk (native) or provided by the extension (WASM). Thread-safe via `Mutex`/`RwLock`; lock ordering is documented in the module header.

**`Document`** : per-file state: rope buffer (for O(log n) position queries), parse result, semantic analysis, lint diagnostics, and (if applicable) parsed package manifest.

## Features

| Feature | Description |
|---------|-------------|
| `fs` *(default: off)* | Enables filesystem workspace scanning on native targets via `rowl/fs`. Required by `dolfin-lsp`. |

Without `fs`, the crate compiles cleanly to `wasm32-unknown-unknown`. Background files on WASM are indexed via the custom `dolfin/indexFiles` LSP notification instead.

## LSP capabilities

| Capability | Method |
|------------|--------|
| Hover | `textDocument/hover` |
| Completion | `textDocument/completion` |
| Semantic tokens | `textDocument/semanticTokens/full` |
| Go-to-definition | `textDocument/definition` |
| Find references | `textDocument/references` |
| Document symbols | `textDocument/documentSymbol` |
| Code actions | `textDocument/codeAction` |
| Rename | `textDocument/prepareRename`, `textDocument/rename` |
| Code lens | `textDocument/codeLens` |
| Diagnostics | `textDocument/publishDiagnostics` (push) |

## Custom notifications

| Method | Direction | Description |
|--------|-----------|-------------|
| `dolfin/indexFiles` | client → server | WASM only. Provides background `.dlf` file content and workspace root so the server can build a cross-file symbol index without filesystem access. |

## Usage

Add the crate to your transport layer:

```toml
# Native (stdio / tower-lsp)
dolfin-lsp-core = { version = "0.1.4", features = ["fs"] }

# WASM Web Worker
dolfin-lsp-core = { version = "0.1.4" }
```

Create one `LspState` per server instance and forward every incoming JSON-RPC message to it:

```rust
use dolfin_lsp_core::dispatch::LspState;

let mut state = LspState::new();

// On each incoming request:
let result = state.handle_request(id, method, params);
// Send result.response back to the client.
// Send each entry in result.notifications as a push notification.

// On each incoming notification:
let result = state.handle_notification(method, params);
// Send each entry in result.notifications as a push notification.
```

## Building

```sh
# Native (requires Rust stable)
cargo build

# WASM
cargo build --target wasm32-unknown-unknown
```

## Testing

```sh
cargo test
```

The test suite includes integration tests that open multiple `.dlf` files in a temporary workspace and verify cross-file diagnostics (e.g. that `semantic/unresolved-type` resolves correctly across files).
