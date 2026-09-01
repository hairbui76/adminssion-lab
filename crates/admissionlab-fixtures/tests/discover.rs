//! Behavioral tests for [`admissionlab_fixtures::discover_fixtures`]
//! (Task 3.1).
//!
//! Named `discover.rs`, not `discovery.rs` as the brief originally
//! named it: controller Ruling R41 (see
//! `.superpowers/sdd/ROADMAP/task-3.1-supplement.md` §1) renamed this
//! file to match this crate's own `discover` module, since Task 3.2
//! separately claims `src/discovery.rs`/`src/resources.rs` for an
//! unrelated job (resolving Kubernetes API resources).
//!
//! # What is proven where
//!
//! - **Determinism that would fail if broken, not vacuously pass.** Every
//!   test below whose name mentions ordering, determinism, or stability
//!   is built so that a plausible wrong implementation (creation-order
//!   traversal, a `HashMap`-keyed slug cache, an id derived from an
//!   absolute path) produces a *different, wrong* result rather than
//!   coincidentally the same one — see each test's own comment for what
//!   the wrong implementation would have produced.
//! - **Content validation** (missing `apiVersion`/`kind`/name,
//!   `generateName` rejection, a non-object document) is tested against
//!   the checked-in fixtures under `testdata/manifests/discovery/`, one
//!   file per failure mode, each selected by its own exact-filename glob
//!   pattern so one file's deliberate defect can never mask another
//!   file's test.
//! - **Ordering** is tested against a *temporary* directory this test
//!   populates itself, in a deliberately scrambled creation order — not
//!   the checked-in `testdata` directory. A checked-in git tree's own
//!   checkout order is not a controlled variable (git stores tree
//!   entries sorted by name, so a naive "did checkout order equal sorted
//!   order" test over checked-in files risks exactly the vacuous trap
//!   this task warns against); a temp directory this test builds gives
//!   full, known control over creation order instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_fixtures::discover_fixtures;
use admissionlab_spec::ResolvedFixtureSelection;
use globset::Glob;

// ---------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------

/// Path to one of the checked-in fixtures under
/// `testdata/manifests/discovery/`, which lives at the workspace root
/// (two levels above this crate's own `CARGO_MANIFEST_DIR`) — mirrors
/// `admissionlab-spec/tests/load.rs`'s own `testdata_config` helper.
fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/manifests/discovery")
}

/// A [`ResolvedFixtureSelection`] whose `root` is `testdata_dir()` and
/// whose only include pattern is `filename` verbatim — no wildcard, so
/// (given every file in `testdata/manifests/discovery/` is flat, with no
/// subdirectories) it can only ever match that one checked-in file,
/// regardless of how many other -- including deliberately malformed --
/// fixtures share the directory.
fn select_testdata_file(filename: &str) -> ResolvedFixtureSelection {
    ResolvedFixtureSelection {
        include: vec![Glob::new(filename).expect("filename is a valid literal glob")],
        root: testdata_dir(),
    }
}

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
/// mirroring `admissionlab-installer/tests/manifests_unit.rs`'s own
/// `unique_temp_dir` helper (each integration test binary is compiled
/// separately, so nothing is actually shared between them).
fn unique_temp_dir(label: &str) -> TempDir {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-fixtures-discover-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    TempDir(dir)
}

/// Writes `contents` to `dir.join(name)`.
fn write_fixture(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write temp fixture file");
}

/// A minimal, valid `ConfigMap` fixture body naming `name`.
fn configmap(name: &str) -> String {
    format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {name}\n")
}

/// A [`ResolvedFixtureSelection`] over every `*.yaml` file directly in
/// `root` (recall `globset`'s default `literal_separator = false`: `*`
/// matches `/` too, so this also reaches nested files, but every test
/// using this helper only ever populates `root` with flat files).
fn select_all_yaml(root: PathBuf) -> ResolvedFixtureSelection {
    ResolvedFixtureSelection {
        include: vec![Glob::new("*.yaml").unwrap()],
        root,
    }
}

// ---------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------

#[test]
fn discovers_a_single_well_formed_fixture() {
    let selection = select_testdata_file("single-fixture.yaml");
    let sources = discover_fixtures(&selection).expect("single-fixture.yaml must discover");

    assert_eq!(sources.len(), 1);
    let source = &sources[0];
    assert_eq!(source.path, testdata_dir().join("single-fixture.yaml"));
    assert_eq!(source.document_index, 0);
    assert_eq!(
        source.sha256, "752d2e1782e6a8724ed7e5c1558b9f1c97b319680f523eb3e15b651613142539",
        "must be the real SHA-256 of single-fixture.yaml's exact on-disk bytes, not a \
         re-serialization -- recompute with `sha256sum` if this file's content ever changes"
    );
    assert_eq!(
        source.object,
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": "single-fixture"},
            "data": {"key": "value"},
        })
    );
    assert_eq!(
        source.id.as_str(),
        "single-fixture-yaml-configmap-single-fixture-0"
    );
}

#[test]
fn namespace_is_folded_into_the_computed_id() {
    let selection = select_testdata_file("namespaced-fixture.yaml");
    let sources = discover_fixtures(&selection).expect("namespaced-fixture.yaml must discover");

    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].id.as_str(),
        "namespaced-fixture-yaml-configmap-admission-lab-test-namespaced-fixture-0"
    );
}

// ---------------------------------------------------------------------
// Empty documents and document_index stability
// ---------------------------------------------------------------------

#[test]
fn document_index_survives_a_skipped_empty_document_without_renumbering() {
    // multi-doc.yaml's first YAML document (index 0) is comment-only --
    // it must be skipped, not turned into a FixtureSource or an error.
    // The mutation this test exists to kill: an implementation that
    // assigns `document_index` by counting only *emitted* FixtureSources
    // (rather than the raw enumerate() position in the document stream)
    // would report 0 and 1 here instead of 1 and 2 -- indistinguishable
    // from the correct answer if this file had no leading empty
    // document, which is exactly why the leading empty document is in
    // the fixture at all.
    let selection = select_testdata_file("multi-doc.yaml");
    let sources = discover_fixtures(&selection).expect("multi-doc.yaml must discover");

    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].document_index, 1);
    assert_eq!(sources[1].document_index, 2);
    assert_eq!(
        sources[0].object["metadata"]["name"],
        serde_json::json!("multi-doc-first")
    );
    assert_eq!(
        sources[1].object["metadata"]["name"],
        serde_json::json!("multi-doc-second")
    );
    assert_ne!(
        sources[0].id, sources[1].id,
        "two distinct documents from the same file must still get distinct ids"
    );
}

#[test]
fn sibling_documents_from_the_same_file_share_the_same_file_level_hash() {
    let selection = select_testdata_file("multi-doc.yaml");
    let sources = discover_fixtures(&selection).expect("multi-doc.yaml must discover");

    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources[0].sha256, sources[1].sha256,
        "sha256 is a whole-file hash (see discover.rs's module documentation, \"Hashing\"), so \
         two documents from the same file must share it"
    );
}

// ---------------------------------------------------------------------
// Rejection rules (brief Step 2 / controller supplement §4)
// ---------------------------------------------------------------------

#[test]
fn missing_api_version_is_rejected() {
    let selection = select_testdata_file("missing-api-version.yaml");
    let err = discover_fixtures(&selection).unwrap_err();
    assert!(matches!(
        err,
        admissionlab_fixtures::FixtureError::MissingField {
            field: "apiVersion",
            ..
        }
    ));
}

#[test]
fn missing_kind_is_rejected() {
    let selection = select_testdata_file("missing-kind.yaml");
    let err = discover_fixtures(&selection).unwrap_err();
    assert!(matches!(
        err,
        admissionlab_fixtures::FixtureError::MissingField { field: "kind", .. }
    ));
}

#[test]
fn missing_name_is_rejected() {
    let selection = select_testdata_file("missing-name.yaml");
    let err = discover_fixtures(&selection).unwrap_err();
    assert!(matches!(
        err,
        admissionlab_fixtures::FixtureError::MissingField {
            field: "metadata.name",
            ..
        }
    ));
}

#[test]
fn generate_name_only_is_rejected_with_a_reason() {
    let selection = select_testdata_file("generate-name-only.yaml");
    let err = discover_fixtures(&selection).unwrap_err();
    assert!(matches!(
        err,
        admissionlab_fixtures::FixtureError::GenerateNameUnsupported { .. }
    ));
    let message = err.to_string();
    assert!(
        message.contains("generateName") && message.contains("deterministic"),
        "message {message:?} must explain *why* generateName is rejected, per controller \
         supplement §4"
    );
}

#[test]
fn a_non_object_document_is_rejected() {
    let selection = select_testdata_file("not-an-object.yaml");
    let err = discover_fixtures(&selection).unwrap_err();
    assert!(matches!(
        err,
        admissionlab_fixtures::FixtureError::NotAnObject {
            found: "an array",
            ..
        }
    ));
}

// ---------------------------------------------------------------------
// Ordering: sorted, not creation order, not filesystem-traversal order
// ---------------------------------------------------------------------

#[test]
fn discovery_order_is_sorted_by_relative_path_not_creation_order() {
    // Created in an order (charlie, alpha, bravo) that differs from
    // sorted order (alpha, bravo, charlie) at every single position --
    // not merely "not already sorted". If `discover_fixtures` returned
    // files in creation order (or in whatever unspecified order
    // `read_dir` happens to yield, which this test does not control or
    // assume), this assertion would see [charlie, alpha, bravo] (or some
    // other non-alphabetical permutation) and fail. See
    // `discover.rs`'s own inline unit tests for a second, filesystem-free
    // proof of the same sort step.
    let dir = unique_temp_dir("ordering");
    write_fixture(dir.path(), "charlie.yaml", &configmap("charlie"));
    write_fixture(dir.path(), "alpha.yaml", &configmap("alpha"));
    write_fixture(dir.path(), "bravo.yaml", &configmap("bravo"));

    let selection = select_all_yaml(dir.path().to_path_buf());
    let sources = discover_fixtures(&selection).expect("all three fixtures must discover");

    let names: Vec<_> = sources
        .iter()
        .map(|s| s.path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["alpha.yaml", "bravo.yaml", "charlie.yaml"]);
}

#[test]
fn sort_key_is_the_relative_path_string_not_paths_own_component_ordering() {
    // `discover.rs`'s own module documentation (Pipeline step 3) names
    // this exact pair: a directory `"a"` holding `x.yaml` (relative path
    // `"a/x.yaml"`) versus a flat file `"a-b.yaml"`. Under `Path`'s
    // component-wise `Ord`, the first components are `"a"` and
    // `"a-b.yaml"` -- `"a"` is a prefix of `"a-b.yaml"`, so `"a"` sorts
    // first, putting `a/x.yaml` before `a-b.yaml`. Under plain
    // byte-lexicographic `str`/`String` `Ord`, the first differing byte
    // is position 1: `-` (0x2D) versus `/` (0x2F), so `a-b.yaml` sorts
    // *first* instead -- the opposite order. This test proves
    // `discover_fixtures` sorts by the `String` relative path, not by
    // `PathBuf`: if a future change swapped the sort key to `PathBuf`
    // (or to the full, `root`-prefixed absolute path, which differs from
    // `root` by the exact same relative suffix and so sorts identically
    // to sorting bare `PathBuf`s), this assertion would see the reversed
    // order and fail.
    let dir = unique_temp_dir("path-vs-string-ordering");
    std::fs::create_dir_all(dir.path().join("a")).expect("create nested dir");
    write_fixture(dir.path(), "a-b.yaml", &configmap("dash"));
    write_fixture(&dir.path().join("a"), "x.yaml", &configmap("nested"));

    // Sanity check on the trap itself, mirroring
    // `admissionlab_recipes::model`'s own
    // `join_confined_naive_starts_with_check_would_have_wrongly_accepted_this`:
    // confirm `Path`'s own `Ord` really does disagree with `str`'s here,
    // so this test would actually distinguish a wrong implementation
    // rather than passing either way regardless of which one is used.
    assert!(
        Path::new("a/x.yaml") < Path::new("a-b.yaml"),
        "sanity check on the trap itself: Path's component-wise Ord must put the nested path \
         first"
    );
    assert!(
        "a/x.yaml" > "a-b.yaml",
        "sanity check on the trap itself: str's byte-lexicographic Ord must put the nested \
         path second"
    );

    let selection = select_all_yaml(dir.path().to_path_buf());
    let sources = discover_fixtures(&selection).expect("both fixtures must discover");

    let relative_order: Vec<_> = sources
        .iter()
        .map(|s| {
            s.path
                .strip_prefix(dir.path())
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(relative_order, ["a-b.yaml", "a/x.yaml"]);
}

#[test]
fn discover_fixtures_is_deterministic_across_repeated_calls() {
    let dir = unique_temp_dir("repeatable");
    write_fixture(dir.path(), "one.yaml", &configmap("one"));
    write_fixture(dir.path(), "two.yaml", &configmap("two"));
    write_fixture(dir.path(), "three.yaml", &configmap("three"));

    let selection = select_all_yaml(dir.path().to_path_buf());
    let first = discover_fixtures(&selection).expect("first call must succeed");
    let second = discover_fixtures(&selection).expect("second call must succeed");

    assert_eq!(
        first, second,
        "calling discover_fixtures twice on the same selection must return byte-for-byte \
         identical results -- any nondeterministic source (HashMap iteration, wall-clock time, \
         a random id) would make this flaky rather than reliably equal"
    );
}

// ---------------------------------------------------------------------
// Stability across machines: never derived from an absolute path
// ---------------------------------------------------------------------

#[test]
fn fixture_id_is_identical_regardless_of_the_selections_absolute_root() {
    // The mutation this test exists to kill: an implementation of
    // `compute_fixture_id` that folded in the *absolute* path (or
    // `selection.root` itself) rather than only the path relative to
    // `root` would make the two ids below different, because
    // `dir_a`/`dir_b` are two distinct, guaranteed-unique absolute
    // directories -- exactly the "stable across machines" property
    // controller supplement §3 requires.
    let dir_a = unique_temp_dir("root-a");
    let dir_b = unique_temp_dir("root-b");
    assert_ne!(
        dir_a.path(),
        dir_b.path(),
        "test precondition: the two roots must differ"
    );
    write_fixture(dir_a.path(), "shared.yaml", &configmap("shared"));
    write_fixture(dir_b.path(), "shared.yaml", &configmap("shared"));

    let sources_a = discover_fixtures(&select_all_yaml(dir_a.path().to_path_buf()))
        .expect("dir_a fixture must discover");
    let sources_b = discover_fixtures(&select_all_yaml(dir_b.path().to_path_buf()))
        .expect("dir_b fixture must discover");

    assert_eq!(sources_a.len(), 1);
    assert_eq!(sources_b.len(), 1);
    assert_eq!(
        sources_a[0].id, sources_b[0].id,
        "identical relative content under two different absolute roots must produce the same \
         fixture id"
    );
    assert_eq!(
        sources_a[0].sha256, sources_b[0].sha256,
        "identical byte content must hash identically regardless of where it lives on disk"
    );
    assert_ne!(
        sources_a[0].path, sources_b[0].path,
        "test precondition: the two FixtureSource.path values are still expected to differ \
         (path is not claimed to be machine-stable, only id and sha256 are)"
    );
}

// ---------------------------------------------------------------------
// Fixture id collisions
// ---------------------------------------------------------------------

#[test]
fn a_real_id_collision_across_two_files_is_rejected_not_silently_accepted() {
    // "a.b.yaml" and "a-b.yaml" are two distinct real files -- distinct
    // relative paths, so `discover_fixtures` never treats them as the
    // same document -- but `compute_fixture_id`'s own `slugify` maps
    // both `.` and `-` to the same separator (see
    // `identity.rs`'s `slugify_is_lossy_by_design_a_dot_and_a_hyphen_collide`
    // unit test), so both slugify their path component to "a-b". Paired
    // with identical kind/name/document_index, this is a genuine,
    // naturally-occurring collision, not a contrived one. Proves
    // `discover_fixtures` end-to-end surfaces
    // `FixtureError::DuplicateFixtureId` rather than silently returning
    // two `FixtureSource`s under one id.
    let dir = unique_temp_dir("collision");
    write_fixture(dir.path(), "a.b.yaml", &configmap("x"));
    write_fixture(dir.path(), "a-b.yaml", &configmap("x"));

    let err = discover_fixtures(&select_all_yaml(dir.path().to_path_buf())).unwrap_err();
    assert!(matches!(
        err,
        admissionlab_fixtures::FixtureError::DuplicateFixtureId { .. }
    ));
}

// ---------------------------------------------------------------------
// Root/selection edge cases
// ---------------------------------------------------------------------

#[test]
fn a_missing_root_directory_is_an_error() {
    let temp_dir = unique_temp_dir("missing-root-parent");
    let root = temp_dir.path().join("does-not-exist");
    assert!(!root.exists(), "test precondition: root must not exist");
    let selection = select_all_yaml(root);

    let err = discover_fixtures(&selection).unwrap_err();
    assert!(matches!(
        err,
        admissionlab_fixtures::FixtureError::WalkDirectory { .. }
    ));
}

#[test]
fn a_pattern_matching_nothing_returns_an_empty_result_not_an_error() {
    let dir = unique_temp_dir("no-matches");
    write_fixture(dir.path(), "irrelevant.txt", "not a fixture");

    let selection = select_all_yaml(dir.path().to_path_buf());
    let sources = discover_fixtures(&selection).expect("no match is not itself an error");
    assert!(sources.is_empty());
}

#[test]
fn an_empty_directory_returns_an_empty_result() {
    let dir = unique_temp_dir("empty-dir");
    let selection = select_all_yaml(dir.path().to_path_buf());
    let sources = discover_fixtures(&selection).expect("an empty directory is not an error");
    assert!(sources.is_empty());
}

// ---------------------------------------------------------------------
// Every FixtureId produced across the checked-in testdata is unique
// ---------------------------------------------------------------------

#[test]
fn every_valid_checked_in_fixture_gets_a_syntactically_valid_and_unique_id() {
    // Discovers every *well-formed* checked-in fixture at once (an
    // explicit list, not a wildcard, so this test does not depend on
    // whether a new deliberately-malformed fixture is later added to
    // the same directory) and checks the ids collected across all of
    // them are pairwise distinct -- exercising `FixtureId`'s own
    // parse-success as a side effect of every discovered id already
    // being one.
    let well_formed = [
        "single-fixture.yaml",
        "namespaced-fixture.yaml",
        "multi-doc.yaml",
    ];
    let mut ids = BTreeSet::new();
    for filename in well_formed {
        let sources = discover_fixtures(&select_testdata_file(filename))
            .unwrap_or_else(|error| panic!("{filename} must discover cleanly: {error}"));
        for source in sources {
            assert!(
                ids.insert(source.id.as_str().to_owned()),
                "id {:?} from {filename} collided with one already seen",
                source.id.as_str()
            );
        }
    }
    assert_eq!(ids.len(), 4, "single (1) + namespaced (1) + multi-doc (2)");
}
