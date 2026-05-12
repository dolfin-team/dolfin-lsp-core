//! Multi-file workspace state.
//!
//! [`World`] owns every open document and a single shared [`SymbolIndex`] that
//! spans all open files **and** (on native) all other `.dlf` files discovered
//! on disk.
//!
//! ## Lock ordering
//!
//! To avoid deadlock, always acquire locks in this order:
//!   1. `index` (write)  — standalone, never while holding `documents`
//!   2. `disk_asts`      — standalone, never while holding `documents`
//!   3. `documents`      — held last; `index` read may be acquired inside
//!                         `reanalyze_all_inner` while this is held, which is
//!                         safe because no other path acquires `index.write`
//!                         while holding `documents`.
//!
//! `workspace_root` is a separate `RwLock` that may be read at any point
//! (no lock held); it is only written once during `scan_workspace`.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use dolfin_analysis::{AnalysisResult, SymbolIndex, analyze_with_index};
use dolfin_diagnostic::Diagnostic;
use dolfin_lint::PackageKnowledge;
use dolfin_lint::engine::LintEngine;
use lsp_types::Url;
use ropey::Rope;
use rowl::ast::QualifiedName;
use rowl::comment::CommentMap;
use rowl::{OntologyFile, PackageFile};
use rowl::error::ParseResult;
use rowl::parser::{parse_ontology, parse_ontology_with_comments};
use tracing::{debug, instrument, warn};

// ── Document ──────────────────────────────────────────────────────────────────

/// Per-document state: rope buffer, parse result, semantic analysis, and lint.
pub struct Document {
    /// Source text as a rope for O(log n) position queries.
    pub rope: Rope,
    /// Raw parse result (may contain errors; AST may be partial).
    pub parse: ParseResult,
    /// Semantic analysis. `None` when the parser produced no AST or when the
    /// document is a `package.dlf` manifest.
    pub analysis: Option<AnalysisResult>,
    /// Lint diagnostics produced by the default rule set.
    pub lint: Vec<Diagnostic>,
    /// Parsed package manifest. Set only when this document is `package.dlf`.
    pub package: Option<PackageFile>,
    /// Namespace derived from the file path relative to the workspace root.
    /// `None` when the workspace root is unknown or the path cannot be mapped.
    pub namespace: Option<QualifiedName>,
    /// Comments attached to AST nodes, used by hover to surface descriptions.
    pub comment_map: CommentMap,
}

// ── World ─────────────────────────────────────────────────────────────────────

/// The workspace: all currently open documents plus their merged symbol index.
pub struct World {
    /// Editor-open documents.
    documents: Mutex<HashMap<Url, Document>>,
    /// Merged symbol index: editor-open *and* background files.
    index: RwLock<SymbolIndex>,
    /// Parsed ASTs for background files, keyed by URI string.
    /// On native: populated by `scan_workspace` via filesystem discovery.
    /// On WASM: populated by `index_background_files` from extension-provided content.
    disk_asts: Mutex<HashMap<String, (QualifiedName, OntologyFile)>>,
    /// Workspace root directory (set once in `scan_workspace`).
    /// Used to derive namespaces for editor-open documents on native.
    #[cfg(not(target_arch = "wasm32"))]
    workspace_root: RwLock<Option<std::path::PathBuf>>,
    /// Workspace root URI sent via `dolfin/indexFiles` (WASM only).
    /// Used to compute relative paths for namespace derivation.
    #[cfg(target_arch = "wasm32")]
    wasm_workspace_root: RwLock<Option<Url>>,
}

impl Default for World {
    fn default() -> Self {
        World {
            documents: Mutex::new(HashMap::new()),
            index: RwLock::new(SymbolIndex::default()),
            disk_asts: Mutex::new(HashMap::new()),
            #[cfg(not(target_arch = "wasm32"))]
            workspace_root: RwLock::new(None),
            #[cfg(target_arch = "wasm32")]
            wasm_workspace_root: RwLock::new(None),
        }
    }
}

impl World {
    pub fn new() -> Self {
        World::default()
    }

    fn uri_key(uri: &Url) -> String {
        uri.as_str().to_owned()
    }

    fn is_package_manifest(uri: &Url) -> bool {
        uri.path().ends_with("/package.dlf") || uri.path() == "package.dlf"
    }

    // ── Workspace scan (native only) ──────────────────────────────────────────

    #[cfg(feature = "fs")]
    pub fn scan_workspace(&self, root: &std::path::Path) {
        // Persist the workspace root so namespace derivation works for
        // subsequently opened editor documents.
        *self.workspace_root.write().unwrap() = Some(root.to_owned());

        let discovered = match rowl::package::discover_ontology_files(root) {
            Ok(files) => files,
            Err(e) => {
                warn!("workspace scan failed: {e}");
                return;
            }
        };

        // Snapshot open URIs without holding the documents lock during I/O.
        let open_uris: std::collections::HashSet<Url> = {
            self.documents.lock().unwrap().keys().cloned().collect()
        };

        let mut new_asts: Vec<(String, QualifiedName, OntologyFile)> = Vec::new();
        for file_info in discovered {
            let Ok(uri) = Url::from_file_path(&file_info.absolute_path) else {
                continue;
            };
            if open_uris.contains(&uri) {
                continue; // editor version takes precedence
            }
            let Ok(source) = std::fs::read_to_string(&file_info.absolute_path) else {
                continue;
            };
            let parse = parse_ontology(&source);
            if let Some(file) = parse.ontology {
                new_asts.push((Self::uri_key(&uri), file_info.derived_namespace, file));
            }
        }

        // Update index and disk_asts (no documents lock held here).
        {
            let mut idx = self.index.write().unwrap();
            let mut disk = self.disk_asts.lock().unwrap();
            for (key, ns, file) in &new_asts {
                idx.add_file(key, file);
                disk.insert(key.clone(), (ns.clone(), file.clone()));
            }
        }

        let mut docs = self.documents.lock().unwrap();
        self.reanalyze_all_inner(&mut docs);
    }

    // ── Background-file indexing (WASM / extension-provided) ─────────────────

    /// Index background files whose content is provided by the extension.
    ///
    /// Called in response to the custom `dolfin/indexFiles` notification.
    /// `root` is the workspace root URI (used to derive namespaces from paths).
    /// Files already open in the editor are skipped — the editor version wins.
    pub fn index_background_files(&self, root: Url, files: Vec<(Url, String)>) {
        #[cfg(target_arch = "wasm32")]
        {
            *self.wasm_workspace_root.write().unwrap() = Some(root.clone());
        }

        let open_uris: std::collections::HashSet<Url> = {
            self.documents.lock().unwrap().keys().cloned().collect()
        };

        let mut new_asts: Vec<(String, QualifiedName, OntologyFile)> = Vec::new();
        for (uri, text) in files {
            if open_uris.contains(&uri) {
                continue; // editor version takes precedence
            }
            let ns = match namespace_relative_to_root(&root, &uri) {
                Some(ns) => ns,
                None => continue,
            };
            let parse = parse_ontology(&text);
            if let Some(file) = parse.ontology {
                new_asts.push((Self::uri_key(&uri), ns, file));
            }
        }

        {
            let mut idx = self.index.write().unwrap();
            let mut disk = self.disk_asts.lock().unwrap();
            for (key, ns, file) in &new_asts {
                idx.add_file(key, file);
                disk.insert(key.clone(), (ns.clone(), file.clone()));
            }
        }

        let mut docs = self.documents.lock().unwrap();
        self.reanalyze_all_inner(&mut docs);
    }

    // ── Disk-file helpers (native only) ──────────────────────────────────────

    #[cfg(not(target_arch = "wasm32"))]
    pub fn reload_from_disk(&self, uri: &Url) {
        if Self::is_package_manifest(uri) {
            return;
        }
        // Skip if the editor has this file open (snapshot check, not held).
        if self.documents.lock().unwrap().contains_key(uri) {
            return;
        }
        let Ok(path) = uri.to_file_path() else { return };
        let Ok(source) = std::fs::read_to_string(&path) else {
            return;
        };
        let parse = parse_ontology(&source);
        let key = Self::uri_key(uri);

        // Derive namespace from the file path relative to workspace root.
        let ns_opt = self.namespace_for_uri(uri);

        {
            let mut idx = self.index.write().unwrap();
            let mut disk = self.disk_asts.lock().unwrap();
            if let (Some(file), Some(ns)) = (parse.ontology, ns_opt) {
                idx.add_file(&key, &file);
                disk.insert(key, (ns, file));
            } else {
                disk.remove(&key);
            }
        }
        let mut docs = self.documents.lock().unwrap();
        self.reanalyze_all_inner(&mut docs);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn remove_disk_file(&self, uri: &Url) {
        if self.documents.lock().unwrap().contains_key(uri) {
            return;
        }
        let key = Self::uri_key(uri);
        {
            self.index.write().unwrap().remove_file(&key);
            self.disk_asts.lock().unwrap().remove(&key);
        }
        let mut docs = self.documents.lock().unwrap();
        self.reanalyze_all_inner(&mut docs);
    }

    // ── Editor document lifecycle ─────────────────────────────────────────────

    #[instrument(skip(self, source), fields(uri = %uri))]
    pub fn open(&self, uri: Url, source: String) {
        debug!(bytes = source.len(), "opening document");
        // Background AST no longer needed — editor version takes over.
        self.disk_asts.lock().unwrap().remove(&Self::uri_key(&uri));

        let doc = self.make_document(&uri, &source);
        self.update_index(&uri, &doc); // acquires + releases index.write
        let mut docs = self.documents.lock().unwrap();
        docs.insert(uri, doc);
        self.reanalyze_all_inner(&mut docs);
    }

    #[instrument(skip(self, source), fields(uri = %uri))]
    pub fn update(&self, uri: &Url, source: String) {
        debug!(bytes = source.len(), "updating document");
        let doc = self.make_document(uri, &source);
        self.update_index(uri, &doc);
        let mut docs = self.documents.lock().unwrap();
        docs.insert(uri.clone(), doc);
        self.reanalyze_all_inner(&mut docs);
    }

    #[instrument(skip(self), fields(uri = %uri))]
    pub fn close(&self, uri: &Url) {
        debug!("closing document");

        // On native: read the disk version to restore background index entry.
        #[cfg(not(target_arch = "wasm32"))]
        let native_restore: Option<(String, QualifiedName, OntologyFile)> =
            if !Self::is_package_manifest(uri) {
                let ns_opt = self.namespace_for_uri(uri);
                uri.to_file_path()
                    .ok()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .and_then(|src| {
                        let parse = parse_ontology(&src);
                        parse
                            .ontology
                            .zip(ns_opt)
                            .map(|(f, ns)| (Self::uri_key(uri), ns, f))
                    })
            } else {
                None
            };

        // On WASM: restore background index entry from the last known document
        // content (the file was provided via dolfin/indexFiles at startup).
        #[cfg(target_arch = "wasm32")]
        let wasm_restore: Option<(String, QualifiedName, OntologyFile)> =
            if !Self::is_package_manifest(uri) {
                let docs = self.documents.lock().unwrap();
                docs.get(uri).and_then(|doc| {
                    let ns = doc.namespace.clone()?;
                    let file = doc.parse.ontology.clone()?;
                    Some((Self::uri_key(uri), ns, file))
                })
            } else {
                None
            };

        // Update index and disk_asts (no documents lock held except the wasm
        // snapshot above which has already been released).
        {
            let mut idx = self.index.write().unwrap();
            idx.remove_file(&Self::uri_key(uri));
            #[cfg(not(target_arch = "wasm32"))]
            if let Some((ref key, _, ref file)) = native_restore {
                idx.add_file(key, file);
            }
            #[cfg(target_arch = "wasm32")]
            if let Some((ref key, _, ref file)) = wasm_restore {
                idx.add_file(key, file);
            }
        }
        {
            let mut disk = self.disk_asts.lock().unwrap();
            #[cfg(not(target_arch = "wasm32"))]
            match native_restore {
                Some((key, ns, file)) => { disk.insert(key, (ns, file)); }
                None => { disk.remove(&Self::uri_key(uri)); }
            }
            #[cfg(target_arch = "wasm32")]
            match wasm_restore {
                Some((key, ns, file)) => { disk.insert(key, (ns, file)); }
                None => { disk.remove(&Self::uri_key(uri)); }
            }
        }

        let mut docs = self.documents.lock().unwrap();
        docs.remove(uri);
        self.reanalyze_all_inner(&mut docs);
    }

    /// Call `f` with a reference to the document for `uri`, if it is open.
    /// The internal lock is held only for the duration of the closure.
    pub fn with_doc<F, R>(&self, uri: &Url, f: F) -> Option<R>
    where
        F: FnOnce(&Document) -> R,
    {
        let docs = self.documents.lock().unwrap();
        let doc = docs.get(uri)?;
        Some(f(doc))
    }

    /// All currently open URIs (snapshot).
    pub fn all_uris(&self) -> Vec<Url> {
        self.documents.lock().unwrap().keys().cloned().collect()
    }

    /// All known ASTs: editor-open documents and background files.
    pub fn all_file_asts(&self) -> Vec<(String, OntologyFile)> {
        let mut result: Vec<(String, OntologyFile)> = Vec::new();

        let disk = self.disk_asts.lock().unwrap();
        for (key, (_ns, file)) in disk.iter() {
            result.push((key.clone(), file.clone()));
        }
        drop(disk);

        let docs = self.documents.lock().unwrap();
        for (uri, doc) in docs.iter() {
            if let Some(file) = doc.parse.ontology.clone() {
                result.push((Self::uri_key(uri), file));
            }
        }
        result
    }

    // ── Internals ─────────────────────────────────────────────────────────────

    fn make_document(&self, uri: &Url, source: &str) -> Document {
        let rope = Rope::from_str(source);
        let namespace = {
            #[cfg(not(target_arch = "wasm32"))]
            { self.namespace_for_uri(uri) }
            #[cfg(target_arch = "wasm32")]
            {
                let root = self.wasm_workspace_root.read().unwrap();
                match root.as_ref() {
                    Some(r) => namespace_relative_to_root(r, uri),
                    None => namespace_from_uri_path(uri),
                }
            }
        };
        if Self::is_package_manifest(uri) {
            let (parse, package) = parse_package_manifest(source);
            Document { rope, parse, analysis: None, lint: vec![], package, namespace, comment_map: CommentMap::default() }
        } else {
            let rowl::parser::ParseWithComments { result: parse, comments } = parse_ontology_with_comments(source);
            let comment_map = parse.ontology.as_ref()
                .map(|ontology| CommentMap::build(ontology, comments))
                .unwrap_or_default();
            Document { rope, parse, analysis: None, lint: vec![], package: None, namespace, comment_map }
        }
    }

    /// Update the shared symbol index after a document has been (re)parsed.
    /// Acquires `index.write` independently — must NOT be called while
    /// `documents` is locked.
    fn update_index(&self, uri: &Url, doc: &Document) {
        let key = Self::uri_key(uri);
        let mut idx = self.index.write().unwrap();
        match &doc.parse.ontology {
            Some(file) => idx.add_file(&key, file),
            None => idx.remove_file(&key),
        }
    }

    /// Re-analyse every open document with the current shared index.
    ///
    /// # Caller contract
    /// Must be called while `documents` is already locked (the caller passes
    /// the `&mut HashMap`).  Acquires `index.read` internally — safe because
    /// no other path acquires `index.write` while holding `documents`.
    fn reanalyze_all_inner(&self, docs: &mut HashMap<Url, Document>) {
        let idx = self.index.read().unwrap().clone();
        let engine = LintEngine::with_default_rules();

        // Build cross-file knowledge from all known ASTs before linting.
        let pkg_knowledge = self.build_package_knowledge(docs);

        for (uri, doc) in docs.iter_mut() {
            if let Some(file) = doc.parse.ontology.clone() {
                doc.analysis = Some(analyze_with_index(file.clone(), idx.clone()));
                let source = doc.rope.to_string();
                let file_key = Self::uri_key(uri);
                doc.lint = engine.check_with_knowledge(&file, &source, &file_key, &pkg_knowledge);
            }
        }
    }

    /// Build a [`PackageKnowledge`] snapshot from all currently known ASTs.
    ///
    /// Combines editor-open documents (passed in, `documents` lock is already
    /// held by the caller) with disk-only files (their lock is acquired briefly
    /// here and released before returning).
    fn build_package_knowledge(&self, docs: &HashMap<Url, Document>) -> PackageKnowledge {
        // Collect (namespace, ast) pairs from editor-open documents.
        let editor_files: Vec<(QualifiedName, OntologyFile)> = docs
            .values()
            .filter_map(|doc| {
                let ns = doc.namespace.clone()?;
                let ast = doc.parse.ontology.clone()?;
                Some((ns, ast))
            })
            .collect();

        // Collect (namespace, ast) from background files.
        let disk_files: Vec<(QualifiedName, OntologyFile)> = self
            .disk_asts
            .lock()
            .unwrap()
            .values()
            .map(|(ns, ast)| (ns.clone(), ast.clone()))
            .collect();

        let mut knowledge = PackageKnowledge::from_asts(
            editor_files
                .iter()
                .chain(disk_files.iter())
                .map(|(ns, ast)| (ns, ast)),
        );

        // Populate all_asts for cross-file rename fixes (editor-open docs).
        for (uri, doc) in docs {
            if let Some(ast) = doc.parse.ontology.clone() {
                knowledge.all_asts.insert(Self::uri_key(uri), ast);
            }
        }

        knowledge
    }

    /// Derive the namespace for a URI from its file path relative to the workspace root.
    ///
    /// Returns `None` if the workspace root is not set, the URI is not a file URI,
    /// or the path cannot be made relative to the root.
    #[cfg(not(target_arch = "wasm32"))]
    fn namespace_for_uri(&self, uri: &Url) -> Option<QualifiedName> {
        let root = self.workspace_root.read().unwrap();
        let root = root.as_ref()?;
        let file_path = uri.to_file_path().ok()?;
        let rel = file_path.strip_prefix(root).ok()?;
        derive_namespace(rel)
    }
}

// ── Namespace derivation ──────────────────────────────────────────────────────

/// Derive a dot-separated namespace from a relative file path.
///
/// Rules (mirrors `rowl::package::discovery::path_to_namespace`):
/// - Directory separators become dots
/// - The `.dlf` extension is stripped from the final component
/// - Each component is lowercased
/// - Invalid (non-alphanumeric/underscore) components return `None`
#[cfg(not(target_arch = "wasm32"))]
fn derive_namespace(rel: &std::path::Path) -> Option<QualifiedName> {
    let mut parts = Vec::new();
    for component in rel.components() {
        if let std::path::Component::Normal(name) = component {
            let s = name.to_str()?;
            let s = s.strip_suffix(".dlf").unwrap_or(s);
            if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            parts.push(s.to_lowercase());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(QualifiedName { parts, span: None })
    }
}

/// Derive a namespace for a file URI by stripping the workspace root prefix
/// and interpreting the remainder as a relative path.
///
/// Works on both native and WASM and is the primary derivation path when a
/// workspace root is known (i.e. after `dolfin/indexFiles` or `scan_workspace`).
fn namespace_relative_to_root(root: &Url, file: &Url) -> Option<QualifiedName> {
    let root_path = root.path().trim_end_matches('/');
    let file_path = file.path();
    let rel = file_path.strip_prefix(root_path)?.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    let parts: Vec<String> = rel
        .split('/')
        .map(|s| s.strip_suffix(".dlf").unwrap_or(s).to_lowercase())
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect();
    if parts.is_empty() { None } else { Some(QualifiedName { parts, span: None }) }
}

/// Derive a namespace from the path component of a virtual `file:///` URI.
///
/// Fallback when no workspace root is known (e.g. playground with relative URIs).
#[cfg(target_arch = "wasm32")]
fn namespace_from_uri_path(uri: &Url) -> Option<QualifiedName> {
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        return None;
    }
    let parts: Vec<String> = path
        .split('/')
        .map(|s| s.strip_suffix(".dlf").unwrap_or(s).to_lowercase())
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(QualifiedName { parts, span: None })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Two files: `human.dlf` defines `Person`, `employee.dlf` references it
    /// via a prefix.  After both are opened, `semantic/unresolved-type` must
    /// NOT fire for `human.Person` and MUST fire for a genuinely unknown type.
    #[test]
    fn cross_file_unresolved_type_resolves() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Write human.dlf
        let human_path = root.join("human.dlf");
        std::fs::write(&human_path, "concept Person:\n  has name: string\n").unwrap();

        // Write employee.dlf — references human.Person via prefix
        let employee_path = root.join("employee.dlf");
        std::fs::write(
            &employee_path,
            "prefix human = human\nconcept Employee:\n  sub human.Person\n",
        )
        .unwrap();

        let world = World::new();
        // Set workspace root so namespace derivation works.
        *world.workspace_root.write().unwrap() = Some(root.to_owned());

        let human_uri = Url::from_file_path(&human_path).unwrap();
        let employee_uri = Url::from_file_path(&employee_path).unwrap();

        world.open(
            human_uri.clone(),
            std::fs::read_to_string(&human_path).unwrap(),
        );
        world.open(
            employee_uri.clone(),
            std::fs::read_to_string(&employee_path).unwrap(),
        );

        let employee_diags: Vec<String> = world
            .with_doc(&employee_uri, |doc| {
                doc.lint
                    .iter()
                    .filter(|d| {
                        d.code
                            == dolfin_diagnostic::DiagnosticCode::Lint(
                                "semantic/unresolved-type".into(),
                            )
                    })
                    .map(|d| d.message.clone())
                    .collect()
            })
            .unwrap_or_default();

        // No unresolved-type for human.Person (cross-file ref that should resolve).
        assert!(
            employee_diags.is_empty(),
            "unexpected unresolved-type diagnostics: {:?}",
            employee_diags
        );

        // Now open a file with a genuinely unknown type.
        let broken_path = root.join("broken.dlf");
        std::fs::write(&broken_path, "concept Broken:\n  has x: DoesNotExist\n").unwrap();
        let broken_uri = Url::from_file_path(&broken_path).unwrap();
        world.open(
            broken_uri.clone(),
            std::fs::read_to_string(&broken_path).unwrap(),
        );

        let broken_diags: Vec<String> = world
            .with_doc(&broken_uri, |doc| {
                doc.lint
                    .iter()
                    .filter(|d| {
                        d.code
                            == dolfin_diagnostic::DiagnosticCode::Lint(
                                "semantic/unresolved-type".into(),
                            )
                    })
                    .map(|d| d.message.clone())
                    .collect()
            })
            .unwrap_or_default();

        assert!(
            !broken_diags.is_empty(),
            "expected unresolved-type for DoesNotExist but got none"
        );
    }
}

// ── Package manifest parsing ──────────────────────────────────────────────────

fn parse_package_manifest(source: &str) -> (ParseResult, Option<PackageFile>) {
    match rowl::parse_package(source) {
        Ok(pkg) => (ParseResult::failure(vec![]), Some(pkg)),
        Err(e) => {
            let diag = (*e).into_diagnostic();
            (ParseResult::failure(vec![diag]), None)
        }
    }
}
