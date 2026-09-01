//! Behavioral tests for explicit parameterized fixture matrices (Task
//! 5.10): [`admissionlab_fixtures::expand_matrix`] on its own, and the
//! whole of [`admissionlab_fixtures::discover_fixtures`] once a
//! `FixtureMatrix` document is in the tree.
//!
//! # What is proven where
//!
//! - **Expansion in isolation** (`expand_matrix` called directly)
//!   against temporary directories this file builds itself, so each
//!   test controls exactly one variable — the base's content, one
//!   patch, one id.
//! - **Discovery integration** (`discover_fixtures` end to end) proves
//!   the properties that only exist once a matrix is one document among
//!   many: where a matrix's cases land in the discovered order, that a
//!   matrix document is never itself replayed as a Kubernetes object,
//!   that a base is replayed exactly when the include globs say so, and
//!   that an expansion failure is a hard error rather than a silent
//!   skip.
//! - **The checked-in `fixtures/core/matrix/` example** is exercised for
//!   real, not merely assumed to parse: the last section discovers it
//!   through the same public entry point a lab run uses and pins what it
//!   produces. A test that only tested temp-directory YAML could pass
//!   while the shipped example was broken.
//!
//! Determinism tests here follow the same discipline as
//! `tests/discover.rs`: each is built so a plausible wrong
//! implementation produces a *different* result rather than
//! coincidentally the same one, and each test's own comment says which
//! wrong implementation it exists to kill.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_fixtures::{FixtureError, FixtureMatrixSpec, discover_fixtures, expand_matrix};
use admissionlab_spec::ResolvedFixtureSelection;
use globset::Glob;

// ---------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------

/// A temporary directory that removes itself when dropped.
///
/// A test holds one for as long as it uses paths underneath it. `Drop`
/// runs on a panicking assertion too, which an explicit delete at the
/// end of a test does not — that is what keeps a `cargo test` run from
/// leaving a directory per test behind in the system temp directory.
struct TempDir(PathBuf);

impl TempDir {
    /// The directory's path, valid for as long as this guard lives.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh, guaranteed-unique directory under the system temp directory,
/// mirroring `tests/discover.rs`'s own helper of the same name.
fn unique_temp_dir(label: &str) -> TempDir {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-fixtures-matrix-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    TempDir(dir)
}

/// Writes `contents` to `dir.join(name)`, creating parent directories.
fn write(dir: &Path, name: &str, contents: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent directory");
    }
    std::fs::write(path, contents).expect("write temp file");
}

/// A minimal, valid base pod fixture.
const BASE_POD: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: base\nspec:\n  containers:\n    - name: app\n      image: registry.k8s.io/pause:3.10\n";

/// Parses a `FixtureMatrix` document's `spec` block from YAML text, so
/// each test can write the matrix exactly as a user would rather than
/// building the struct field by field.
fn matrix_spec(yaml: &str) -> FixtureMatrixSpec {
    let value: serde_json::Value = serde_norway::from_str(yaml).expect("valid YAML");
    serde_json::from_value(value["spec"].clone()).expect("valid FixtureMatrixSpec")
}

/// A one-case matrix over `base.yaml` whose single case applies
/// `patches_yaml` (already indented for a YAML block sequence).
fn single_case_matrix(patches_yaml: &str) -> FixtureMatrixSpec {
    matrix_spec(&format!(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: FixtureMatrix\n\
         spec:\n\
        \x20 id: m\n\
        \x20 base: base.yaml\n\
        \x20 cases:\n\
        \x20   - id: c\n\
        \x20     patches:\n{patches_yaml}"
    ))
}

/// A [`ResolvedFixtureSelection`] over every `*.yaml` file under `root`
/// — recall `globset`'s default `literal_separator = false`, so `*`
/// matches `/` too and this reaches nested files as well.
fn select_all_yaml(root: PathBuf) -> ResolvedFixtureSelection {
    ResolvedFixtureSelection {
        include: vec![Glob::new("*.yaml").unwrap()],
        root,
    }
}

/// The checked-in example directory, `fixtures/core/matrix/`, two levels
/// above this crate's own `CARGO_MANIFEST_DIR` — mirrors
/// `tests/discover.rs`'s own `testdata_dir` helper.
///
/// Deliberately the example *directory* rather than the repository root:
/// [`discover_fixtures`] walks its whole `root`, and a walk rooted at the
/// repository would traverse `target/` on every run.
fn example_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/core/matrix")
}

/// Renders `error`'s whole `Display` chain, so a test can assert on the
/// underlying cause a wrapping variant carries in its `#[source]`.
fn error_chain(error: &FixtureError) -> String {
    let mut out = error.to_string();
    let mut current: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(source) = current {
        out.push_str(": ");
        out.push_str(&source.to_string());
        current = source.source();
    }
    out
}

// ---------------------------------------------------------------------
// expand_matrix: happy path
// ---------------------------------------------------------------------

#[test]
fn expands_one_fixture_per_case_in_declaration_order() {
    let root = unique_temp_dir("order");
    write(root.path(), "base.yaml", BASE_POD);
    // Case ids in a deliberately *non*-alphabetical order: an
    // implementation that sorted or hash-mapped the cases would produce
    // `[alpha, mid, zulu]` and fail this, where a declaration-order one
    // produces `[zulu, alpha, mid]`.
    let spec = matrix_spec(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: FixtureMatrix\n\
         spec:\n\
        \x20 id: m\n\
        \x20 base: base.yaml\n\
        \x20 cases:\n\
        \x20   - id: zulu\n\
        \x20     patches: []\n\
        \x20   - id: alpha\n\
        \x20     patches: []\n\
        \x20   - id: mid\n\
        \x20     patches: []\n",
    );

    let sources = expand_matrix(&spec, root.path()).expect("expansion must succeed");
    let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["m-zulu", "m-alpha", "m-mid"]);
}

#[test]
fn a_case_patch_is_applied_to_the_parsed_base_object() {
    let root = unique_temp_dir("apply");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: add\n\
        \x20         path: /spec/hostNetwork\n\
        \x20         value: true\n",
    );

    let sources = expand_matrix(&spec, root.path()).expect("expansion must succeed");
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].object["spec"]["hostNetwork"],
        serde_json::json!(true)
    );
    // The rest of the base survives untouched -- a patch varies one
    // field, it does not rebuild the document.
    assert_eq!(
        sources[0].object["metadata"]["name"],
        serde_json::json!("base")
    );
    assert_eq!(
        sources[0].object["spec"]["containers"][0]["image"],
        serde_json::json!("registry.k8s.io/pause:3.10")
    );
}

/// The base object must not be mutated between cases: case two starts
/// from the base, not from case one's result. The mutation this kills:
/// patching a single shared `Value` in place instead of a clone per
/// case.
#[test]
fn each_case_starts_from_the_unmodified_base_not_the_previous_case() {
    let root = unique_temp_dir("isolation");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = matrix_spec(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: FixtureMatrix\n\
         spec:\n\
        \x20 id: m\n\
        \x20 base: base.yaml\n\
        \x20 cases:\n\
        \x20   - id: first\n\
        \x20     patches:\n\
        \x20       - op: add\n\
        \x20         path: /spec/hostNetwork\n\
        \x20         value: true\n\
        \x20   - id: second\n\
        \x20     patches:\n\
        \x20       - op: add\n\
        \x20         path: /spec/serviceAccountName\n\
        \x20         value: runner\n",
    );

    let sources = expand_matrix(&spec, root.path()).expect("expansion must succeed");
    assert_eq!(
        sources[1].object["spec"]["serviceAccountName"],
        serde_json::json!("runner")
    );
    assert!(
        sources[1].object["spec"].get("hostNetwork").is_none(),
        "case `second` must not inherit case `first`'s patch: {:?}",
        sources[1].object
    );
}

#[test]
fn an_expanded_fixture_points_at_the_base_file_and_its_document_index() {
    let root = unique_temp_dir("provenance");
    // A leading empty document, so `document_index` cannot coincidentally
    // be 0: an implementation that hard-coded 0 rather than recording the
    // base document's true position would fail this.
    write(
        root.path(),
        "base.yaml",
        &format!("---\n# only a comment\n---\n{BASE_POD}"),
    );
    let spec = single_case_matrix("\x20       []\n");

    let sources = expand_matrix(&spec, root.path()).expect("expansion must succeed");
    assert_eq!(sources[0].path, root.path().join("base.yaml"));
    assert_eq!(sources[0].document_index, 1);
}

// ---------------------------------------------------------------------
// expand_matrix: source hash
// ---------------------------------------------------------------------

#[test]
fn expansion_is_byte_for_byte_deterministic_across_calls() {
    let root = unique_temp_dir("deterministic");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: add\n\
        \x20         path: /metadata/labels\n\
        \x20         value: {b: \"2\", a: \"1\"}\n",
    );

    assert_eq!(
        expand_matrix(&spec, root.path()).unwrap(),
        expand_matrix(&spec, root.path()).unwrap()
    );
}

/// The hash must cover the base file's bytes: editing the base changes
/// every case's hash even though no patch changed. The mutation this
/// kills: hashing only the patch list.
#[test]
fn the_source_hash_changes_when_the_base_file_changes() {
    let root = unique_temp_dir("hash-base");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix("\x20       []\n");
    let before = expand_matrix(&spec, root.path()).unwrap()[0].sha256.clone();

    write(
        root.path(),
        "base.yaml",
        &BASE_POD.replace("pause:3.10", "pause:3.9"),
    );
    let after = expand_matrix(&spec, root.path()).unwrap()[0].sha256.clone();

    assert_ne!(before, after);
}

/// ...and it must cover the patch list: two cases over one base differ.
/// The mutation this kills: passing the base file's own hash straight
/// through, which would make every case of a matrix indistinguishable in
/// the run manifest.
#[test]
fn the_source_hash_differs_between_cases_over_the_same_base() {
    let root = unique_temp_dir("hash-patches");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = matrix_spec(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: FixtureMatrix\n\
         spec:\n\
        \x20 id: m\n\
        \x20 base: base.yaml\n\
        \x20 cases:\n\
        \x20   - id: one\n\
        \x20     patches:\n\
        \x20       - op: add\n\
        \x20         path: /spec/hostNetwork\n\
        \x20         value: true\n\
        \x20   - id: two\n\
        \x20     patches:\n\
        \x20       - op: add\n\
        \x20         path: /spec/hostNetwork\n\
        \x20         value: false\n",
    );

    let sources = expand_matrix(&spec, root.path()).unwrap();
    assert_ne!(sources[0].sha256, sources[1].sha256);
}

/// A matrix hash must never be mistakable for a file hash of the base:
/// the domain separator exists precisely so a 64-character hex string in
/// a run manifest means one thing.
#[test]
fn the_source_hash_is_not_the_base_files_own_hash() {
    let root = unique_temp_dir("hash-domain");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix("\x20       []\n");

    let expanded = expand_matrix(&spec, root.path()).unwrap()[0].sha256.clone();
    let base_file_hash = discover_fixtures(&ResolvedFixtureSelection {
        include: vec![Glob::new("base.yaml").unwrap()],
        root: root.path().to_path_buf(),
    })
    .unwrap()[0]
        .sha256
        .clone();

    assert_ne!(expanded, base_file_hash);
    assert_eq!(expanded.len(), 64, "still a SHA-256 in lowercase hex");
}

// ---------------------------------------------------------------------
// expand_matrix: rejection
// ---------------------------------------------------------------------

/// Every rejection test asserts on the rendered message rather than only
/// on `is_err()`, so a wrong-but-still-failing implementation (one that
/// rejected everything, or rejected for the wrong reason) cannot pass.
fn expect_matrix_error(spec: &FixtureMatrixSpec, root: &Path, expected: &str) -> String {
    let err = expand_matrix(spec, root).expect_err("expansion must be rejected");
    assert!(
        matches!(err, FixtureError::Matrix(_)),
        "expected FixtureError::Matrix, got {err:?}"
    );
    let message = error_chain(&err);
    assert!(
        message.contains(expected),
        "message {message:?} must mention {expected:?}"
    );
    message
}

#[test]
fn a_duplicate_case_id_is_rejected() {
    let root = unique_temp_dir("dup-case");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = matrix_spec(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: FixtureMatrix\n\
         spec:\n\
        \x20 id: m\n\
        \x20 base: base.yaml\n\
        \x20 cases:\n\
        \x20   - id: same\n\
        \x20     patches: []\n\
        \x20   - id: other\n\
        \x20     patches: []\n\
        \x20   - id: same\n\
        \x20     patches: []\n",
    );
    expect_matrix_error(&spec, root.path(), "declared twice");
}

#[test]
fn an_empty_case_list_is_rejected_rather_than_silently_contributing_nothing() {
    let root = unique_temp_dir("no-cases");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = matrix_spec(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: FixtureMatrix\n\
         spec:\n\
        \x20 id: m\n\
        \x20 base: base.yaml\n\
        \x20 cases: []\n",
    );
    expect_matrix_error(&spec, root.path(), "declares no cases");
}

#[test]
fn a_matrix_id_outside_the_fixture_id_character_set_is_rejected_not_slugified() {
    let root = unique_temp_dir("bad-matrix-id");
    write(root.path(), "base.yaml", BASE_POD);
    let mut spec = single_case_matrix("\x20       []\n");
    spec.id = "Pod_Variants".to_owned();
    expect_matrix_error(&spec, root.path(), "not a valid fixture identifier");
}

#[test]
fn a_case_id_outside_the_fixture_id_character_set_is_rejected_not_slugified() {
    let root = unique_temp_dir("bad-case-id");
    write(root.path(), "base.yaml", BASE_POD);
    let mut spec = single_case_matrix("\x20       []\n");
    spec.cases[0].id = "Host Network".to_owned();
    expect_matrix_error(&spec, root.path(), "not a valid fixture identifier");
}

#[test]
fn a_base_escaping_the_fixture_root_is_rejected() {
    let root = unique_temp_dir("escape");
    write(root.path(), "base.yaml", BASE_POD);
    let mut spec = single_case_matrix("\x20       []\n");
    spec.base = PathBuf::from("../base.yaml");
    expect_matrix_error(&spec, root.path(), "ordinary path components");
}

#[test]
fn an_absolute_base_is_rejected() {
    let root = unique_temp_dir("absolute");
    write(root.path(), "base.yaml", BASE_POD);
    let mut spec = single_case_matrix("\x20       []\n");
    spec.base = root.path().join("base.yaml");
    expect_matrix_error(&spec, root.path(), "ordinary path components");
}

#[test]
fn a_missing_base_file_is_rejected() {
    let root = unique_temp_dir("missing-base");
    let spec = single_case_matrix("\x20       []\n");
    expect_matrix_error(&spec, root.path(), "failed to read base");
}

#[test]
fn a_multi_document_base_file_is_rejected_as_ambiguous() {
    let root = unique_temp_dir("multi-base");
    write(
        root.path(),
        "base.yaml",
        &format!("{BASE_POD}---\n{BASE_POD}"),
    );
    let spec = single_case_matrix("\x20       []\n");
    expect_matrix_error(&spec, root.path(), "exactly one document, found 2");
}

#[test]
fn an_empty_base_file_is_rejected() {
    let root = unique_temp_dir("empty-base");
    write(root.path(), "base.yaml", "# nothing but a comment\n");
    let spec = single_case_matrix("\x20       []\n");
    expect_matrix_error(&spec, root.path(), "exactly one document, found 0");
}

/// RFC 6902 has no valid `add` for a pointer whose parent does not
/// exist, so this is a real patch failure, not a silently-ignored no-op.
#[test]
fn a_patch_against_a_nonexistent_location_is_rejected() {
    let root = unique_temp_dir("bad-pointer");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: add\n\
        \x20         path: /spec/nope/deeper\n\
        \x20         value: 1\n",
    );
    expect_matrix_error(&spec, root.path(), "patch failed");
}

#[test]
fn a_failing_test_operation_is_rejected() {
    let root = unique_temp_dir("test-op");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: test\n\
        \x20         path: /metadata/name\n\
        \x20         value: not-the-name\n",
    );
    expect_matrix_error(&spec, root.path(), "patch failed");
}

/// The point of Step 1/Step 4: an expanded object goes through the exact
/// validation a static fixture does. Removing `kind` must be the same
/// `missing required field "kind"` a file on disk would have produced --
/// asserted through the error *chain*, so this also proves the check is
/// delegated rather than restated.
#[test]
fn a_patch_that_removes_kind_is_rejected_by_the_static_fixture_validation() {
    let root = unique_temp_dir("remove-kind");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: remove\n\
        \x20         path: /kind\n",
    );
    let message = expect_matrix_error(&spec, root.path(), "not a valid fixture");
    assert!(
        message.contains("missing required field \"kind\""),
        "message {message:?} must carry the same static-fixture validation failure"
    );
}

#[test]
fn a_patch_that_empties_metadata_name_is_rejected() {
    let root = unique_temp_dir("empty-name");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: replace\n\
        \x20         path: /metadata/name\n\
        \x20         value: \"\"\n",
    );
    let message = expect_matrix_error(&spec, root.path(), "not a valid fixture");
    assert!(message.contains("metadata.name"), "message {message:?}");
}

/// A patch that swaps `metadata.name` for `metadata.generateName` must
/// hit Alpha's deterministic-name rule, not sneak past it -- the exact
/// rule static fixtures are held to.
#[test]
fn a_patch_introducing_generate_name_only_is_rejected() {
    let root = unique_temp_dir("generate-name");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: remove\n\
        \x20         path: /metadata/name\n\
        \x20       - op: add\n\
        \x20         path: /metadata/generateName\n\
        \x20         value: base-\n",
    );
    let message = expect_matrix_error(&spec, root.path(), "not a valid fixture");
    assert!(message.contains("generateName"), "message {message:?}");
}

/// Replacing the whole document with a scalar is the "patches that
/// produce an invalid object" case in its most literal form.
#[test]
fn a_patch_that_replaces_the_whole_document_with_a_scalar_is_rejected() {
    let root = unique_temp_dir("scalar");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: replace\n\
        \x20         path: \"\"\n\
        \x20         value: just-a-string\n",
    );
    let message = expect_matrix_error(&spec, root.path(), "not a valid fixture");
    assert!(message.contains("a string"), "message {message:?}");
}

/// `json_patch::patch` is atomic, so a later failing operation must not
/// leave an earlier one applied -- and, more importantly here, must not
/// yield a fixture at all.
#[test]
fn a_case_whose_second_patch_fails_yields_no_fixture_at_all() {
    let root = unique_temp_dir("atomic");
    write(root.path(), "base.yaml", BASE_POD);
    let spec = single_case_matrix(
        "\x20       - op: add\n\
        \x20         path: /spec/hostNetwork\n\
        \x20         value: true\n\
        \x20       - op: remove\n\
        \x20         path: /spec/doesNotExist\n",
    );
    expect_matrix_error(&spec, root.path(), "patch failed");
}

// ---------------------------------------------------------------------
// Discovery integration
// ---------------------------------------------------------------------

/// A matrix declaration file, ready to write into a temp fixture tree.
fn matrix_document(id: &str, base: &str, case_id: &str) -> String {
    format!(
        "apiVersion: admissionlab.io/v1alpha1\n\
         kind: FixtureMatrix\n\
         spec:\n\
        \x20 id: {id}\n\
        \x20 base: {base}\n\
        \x20 cases:\n\
        \x20   - id: {case_id}\n\
        \x20     patches:\n\
        \x20       - op: replace\n\
        \x20         path: /metadata/name\n\
        \x20         value: {id}-{case_id}\n"
    )
}

#[test]
fn discovery_expands_a_matrix_and_never_replays_the_matrix_document_itself() {
    let root = unique_temp_dir("discover-basic");
    write(root.path(), "base.yaml", BASE_POD);
    write(
        root.path(),
        "m.matrix.yaml",
        &matrix_document("m", "base.yaml", "c"),
    );

    let sources =
        discover_fixtures(&select_all_yaml(root.path().to_path_buf())).expect("discovery");
    let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();

    // Exactly two: the base (which the `**/*.yaml` include independently
    // matches) and the one expanded case. The matrix document itself is
    // *not* among them -- an implementation that fell through to static
    // validation would either add a third entry here or fail discovery
    // outright with a missing-`metadata.name` error.
    assert_eq!(ids.len(), 2, "discovered {ids:?}");
    assert!(ids.contains(&"m-c"), "discovered {ids:?}");
    assert!(
        sources.iter().all(|s| s.object["kind"] != "FixtureMatrix"),
        "a FixtureMatrix declaration must never be replayed as an object"
    );
}

/// The base-replay rule, stated as a runnable test: the include list is
/// the whole rule, and `base:` neither adds nor removes a file from it.
/// Here the include pattern does *not* reach the base, so only the case
/// is discovered -- the mutation this kills is an implementation that
/// added a matrix's base to the corpus automatically.
#[test]
fn a_base_outside_the_include_globs_is_a_template_only_and_is_not_replayed() {
    let root = unique_temp_dir("template-only");
    write(root.path(), "bases/pod.yaml", BASE_POD);
    write(
        root.path(),
        "m.matrix.yaml",
        &matrix_document("m", "bases/pod.yaml", "c"),
    );

    let selection = ResolvedFixtureSelection {
        // An exact filename: it selects the matrix declaration and
        // nothing else, so `bases/pod.yaml` is genuinely outside the
        // include list rather than incidentally unmatched.
        include: vec![Glob::new("m.matrix.yaml").unwrap()],
        root: root.path().to_path_buf(),
    };
    let sources = discover_fixtures(&selection).expect("discovery");
    let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();

    assert_eq!(ids, ["m-c"], "only the expanded case is replayed");
}

/// The mirror of the test above: when the include globs *do* reach the
/// base, it is replayed in its own right, alongside the cases.
#[test]
fn a_base_matched_by_the_include_globs_is_replayed_alongside_its_cases() {
    let root = unique_temp_dir("base-replayed");
    write(root.path(), "base.yaml", BASE_POD);
    write(
        root.path(),
        "m.matrix.yaml",
        &matrix_document("m", "base.yaml", "c"),
    );

    let sources =
        discover_fixtures(&select_all_yaml(root.path().to_path_buf())).expect("discovery");
    assert!(
        sources
            .iter()
            .any(|s| s.id.as_str() == "base-yaml-pod-base-0"),
        "discovered {:?}",
        sources.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()
    );
}

/// Ordering: a matrix's cases occupy the matrix document's own position
/// in the byte-lexicographic file order, not a block appended at the
/// end. `a.yaml` < `m.matrix.yaml` < `z.yaml` as strings, so the wrong
/// implementations (append-at-end, or expand-first) both produce a
/// different sequence than this asserts.
#[test]
fn matrix_cases_land_at_the_matrix_documents_own_position_in_the_order() {
    let root = unique_temp_dir("splice");
    write(
        root.path(),
        "a.yaml",
        &BASE_POD.replace("name: base", "name: aaa"),
    );
    write(
        root.path(),
        "z.yaml",
        &BASE_POD.replace("name: base", "name: zzz"),
    );
    write(root.path(), "n-base.yaml", BASE_POD);
    write(
        root.path(),
        "n.matrix.yaml",
        &matrix_document("mm", "n-base.yaml", "c"),
    );

    let selection = ResolvedFixtureSelection {
        include: vec![
            Glob::new("a.yaml").unwrap(),
            Glob::new("z.yaml").unwrap(),
            Glob::new("n.matrix.yaml").unwrap(),
        ],
        root: root.path().to_path_buf(),
    };
    let sources = discover_fixtures(&selection).expect("discovery");
    let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();

    assert_eq!(ids, ["a-yaml-pod-aaa-0", "mm-c", "z-yaml-pod-zzz-0"]);
}

#[test]
fn two_matrices_sharing_an_id_are_rejected() {
    let root = unique_temp_dir("dup-matrix-id");
    write(root.path(), "base.yaml", BASE_POD);
    write(
        root.path(),
        "a.matrix.yaml",
        &matrix_document("same", "base.yaml", "one"),
    );
    write(
        root.path(),
        "b.matrix.yaml",
        &matrix_document("same", "base.yaml", "two"),
    );

    let err = discover_fixtures(&select_all_yaml(root.path().to_path_buf()))
        .expect_err("must be rejected");
    let message = error_chain(&err);
    assert!(
        message.contains("is not unique") && message.contains("a.matrix.yaml"),
        "message {message:?} must name the first declaration"
    );
}

/// The `-` separator's known cost, made visible: `(matrix "a-b", case
/// "c")` and `(matrix "a", case "b-c")` both render `a-b-c`. That is not
/// silently accepted -- the whole-corpus duplicate check catches it.
#[test]
fn two_matrices_whose_case_ids_collide_after_joining_are_rejected() {
    let root = unique_temp_dir("id-collision");
    write(root.path(), "base.yaml", BASE_POD);
    write(
        root.path(),
        "a.matrix.yaml",
        &matrix_document("a-b", "base.yaml", "c"),
    );
    write(
        root.path(),
        "b.matrix.yaml",
        &matrix_document("a", "base.yaml", "b-c"),
    );

    let err = discover_fixtures(&ResolvedFixtureSelection {
        include: vec![Glob::new("*.matrix.yaml").unwrap()],
        root: root.path().to_path_buf(),
    })
    .expect_err("must be rejected");
    assert!(
        matches!(err, FixtureError::DuplicateFixtureId { .. }),
        "expected DuplicateFixtureId, got {err:?}"
    );
    assert!(
        error_chain(&err).contains("\"a-b-c\""),
        "{}",
        error_chain(&err)
    );
}

/// Step 6's hard requirement: a broken matrix stops discovery. The
/// mutation this kills is any implementation that logged and continued,
/// which would replay `good.yaml` and report a clean, quietly incomplete
/// run.
#[test]
fn a_broken_matrix_fails_discovery_rather_than_being_skipped() {
    let root = unique_temp_dir("no-skip");
    write(root.path(), "base.yaml", BASE_POD);
    write(
        root.path(),
        "good.yaml",
        &BASE_POD.replace("name: base", "name: good"),
    );
    write(
        root.path(),
        "broken.matrix.yaml",
        "apiVersion: admissionlab.io/v1alpha1\nkind: FixtureMatrix\nspec:\n  id: m\n  base: base.yaml\n  cases:\n    - id: c\n      patches:\n        - op: remove\n          path: /apiVersion\n",
    );

    let err = discover_fixtures(&select_all_yaml(root.path().to_path_buf()))
        .expect_err("must be rejected");
    assert!(
        matches!(err, FixtureError::Matrix(_)),
        "expected FixtureError::Matrix, got {err:?}"
    );
}

/// A near-miss `kind` under this project's own group/version is caught
/// with a message about matrices, rather than falling through to static
/// validation and being reported as a fixture with no `metadata.name`.
#[test]
fn a_misspelled_fixture_matrix_kind_is_rejected_with_a_matrix_message() {
    let root = unique_temp_dir("typo-kind");
    write(
        root.path(),
        "typo.yaml",
        "apiVersion: admissionlab.io/v1alpha1\nkind: Fixturematrix\nspec:\n  id: m\n  base: b.yaml\n  cases: []\n",
    );

    let err = discover_fixtures(&select_all_yaml(root.path().to_path_buf()))
        .expect_err("must be rejected");
    let message = error_chain(&err);
    assert!(
        message.contains("is not a fixture matrix"),
        "message {message:?}"
    );
}

#[test]
fn a_matrix_base_escaping_its_own_directory_is_rejected_at_discovery() {
    let root = unique_temp_dir("discover-escape");
    write(
        root.path(),
        "m.matrix.yaml",
        &matrix_document("m", "../outside.yaml", "c"),
    );

    let err = discover_fixtures(&select_all_yaml(root.path().to_path_buf()))
        .expect_err("must be rejected");
    assert!(
        error_chain(&err).contains("ordinary path components"),
        "{}",
        error_chain(&err)
    );
}

#[test]
fn discovery_over_a_matrix_is_deterministic_across_repeated_calls() {
    let root = unique_temp_dir("discover-deterministic");
    write(root.path(), "base.yaml", BASE_POD);
    write(
        root.path(),
        "m.matrix.yaml",
        &matrix_document("m", "base.yaml", "c"),
    );
    let selection = select_all_yaml(root.path().to_path_buf());

    assert_eq!(
        discover_fixtures(&selection).unwrap(),
        discover_fixtures(&selection).unwrap()
    );
}

// ---------------------------------------------------------------------
// The checked-in fixtures/core/matrix/ example
// ---------------------------------------------------------------------

/// Discovers the shipped example through the same public entry point a
/// lab run uses.
fn discover_checked_in_example() -> Vec<admissionlab_fixtures::FixtureSource> {
    let selection = ResolvedFixtureSelection {
        include: vec![Glob::new("*.yaml").unwrap()],
        root: example_dir(),
    };
    discover_fixtures(&selection).expect("fixtures/core/matrix/ must discover cleanly")
}

#[test]
fn the_checked_in_example_expands_to_its_three_variants_plus_its_base() {
    let sources = discover_checked_in_example();
    let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();

    // The base is matched by `*.yaml` too, so it is replayed in its own
    // right -- exactly what `pod-base.yaml`'s own comments say, and the
    // "yes" branch of the base-replay rule. The order is the discovery
    // order: `pod-base.yaml` sorts before `pod-variants.matrix.yaml`,
    // and the three cases follow in the order they are written.
    assert_eq!(
        ids,
        [
            "pod-base-yaml-pod-admissionlab-fixtures-matrix-pod-base-0",
            "pod-variants-pre-existing-init-container",
            "pod-variants-host-network",
            "pod-variants-custom-service-account",
        ],
        "discovered {ids:?}"
    );
}

#[test]
fn the_checked_in_examples_three_variants_carry_the_fields_they_advertise() {
    let sources = discover_checked_in_example();
    let by_id = |id: &str| {
        sources
            .iter()
            .find(|s| s.id.as_str() == id)
            .unwrap_or_else(|| panic!("no fixture {id}"))
            .object
            .clone()
    };

    let init = by_id("pod-variants-pre-existing-init-container");
    assert_eq!(
        init["spec"]["initContainers"][0]["name"],
        serde_json::json!("pre-existing")
    );
    assert_eq!(
        init["metadata"]["name"],
        serde_json::json!("matrix-pod-pre-existing-init-container")
    );

    assert_eq!(
        by_id("pod-variants-host-network")["spec"]["hostNetwork"],
        serde_json::json!(true)
    );
    assert_eq!(
        by_id("pod-variants-custom-service-account")["spec"]["serviceAccountName"],
        serde_json::json!("matrix-custom-runner")
    );

    // Every variant kept the base's namespace and container: the cases
    // vary one field each, they do not restate the pod.
    for id in [
        "pod-variants-pre-existing-init-container",
        "pod-variants-host-network",
        "pod-variants-custom-service-account",
    ] {
        let object = by_id(id);
        assert_eq!(
            object["metadata"]["namespace"],
            serde_json::json!("admissionlab-fixtures")
        );
        assert_eq!(
            object["spec"]["containers"][0]["name"],
            serde_json::json!("app")
        );
    }
}

/// Each shipped variant must have its own provenance hash: a run
/// manifest that recorded one hash for all four would be useless.
#[test]
fn the_checked_in_examples_fixtures_all_have_distinct_source_hashes() {
    let sources = discover_checked_in_example();
    let mut hashes: Vec<&str> = sources.iter().map(|s| s.sha256.as_str()).collect();
    hashes.sort_unstable();
    let unique = hashes.len();
    hashes.dedup();
    assert_eq!(hashes.len(), unique, "source hashes must all differ");
}
