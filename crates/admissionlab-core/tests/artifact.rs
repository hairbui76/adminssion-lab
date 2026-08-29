//! Behavioral tests for [`ArtifactStore`]: run workspace creation and
//! atomic artifact writes.
//!
//! Every test here is ultimately in service of the guarantees
//! `ArtifactStore`'s own documentation makes: a reader never observes a
//! partially written file, a failed write never leaves a stray temporary
//! file behind, kubeconfig-bearing paths get owner-only permissions on
//! Unix, and a write destination that resolves outside the store's root
//! is rejected rather than silently honored.

use std::path::PathBuf;

use admissionlab_core::{ArtifactError, ArtifactStore, RunId};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// A fresh `tokio` runtime for driving one test's async calls.
///
/// Mirrors `tests/process.rs`'s hand-rolled runtime construction rather
/// than adopting `#[tokio::test]`, which would need a new `tokio`
/// feature (`macros`) this crate's production code has no other reason
/// to depend on.
fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
}

/// A fresh, guaranteed-unique scratch directory under the OS temp dir, to
/// use as one test's artifact store root. Mirrors the unique-temp-dir
/// pattern already established in `tests/domain.rs` and
/// `tests/process.rs`, rather than pulling in a new dependency just for
/// test-only temp directories.
fn unique_store_root(label: &str) -> PathBuf {
    let unique = RunId::generate();
    std::env::temp_dir().join(format!(
        "admissionlab-core-artifact-test-{label}-{}",
        unique.as_str()
    ))
}

/// Reads a path's permission bits (the low 9 bits of its mode), masking
/// off the file-type bits `std::fs::Metadata::permissions` otherwise
/// includes.
#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .unwrap_or_else(|err| panic!("stat {}: {err}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

/// The sorted names of `dir`'s direct children, for asserting exactly
/// what a directory contains (and, just as importantly, does not
/// contain — such as a leftover temporary file).
fn sorted_entry_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read_dir {}: {err}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|err| panic!("dir entry in {}: {err}", dir.display()))
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SampleManifest {
    run_id: String,
    fixture_count: u32,
}

/// A type whose `Serialize` implementation writes part of a struct and
/// then deliberately fails, so the "mid-serialization failure" test
/// below exercises the real `serde_json` code path -- a struct that
/// legitimately starts encoding before erroring out -- rather than
/// mocking the filesystem or short-circuiting before any encoding work
/// happens.
struct FailsPartwayThroughSerialization;

impl Serialize for FailsPartwayThroughSerialization {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct as _;
        let mut state = serializer.serialize_struct("FailsPartwayThroughSerialization", 2)?;
        state.serialize_field("first", "this much gets encoded")?;
        Err(serde::ser::Error::custom(
            "simulated failure partway through serialization",
        ))
    }
}

// ---------------------------------------------------------------------
// create_run -- directory creation (brief Step 1 / Step 2)
// ---------------------------------------------------------------------

#[test]
fn create_run_creates_every_directory_run_paths_names() {
    let rt = test_runtime();
    let root = unique_store_root("create-run-dirs");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();

    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    for dir in [
        paths.root(),
        paths.raw(),
        paths.normalized(),
        paths.reports(),
        paths.logs(),
        paths.kubeconfigs(),
    ] {
        assert!(dir.is_dir(), "expected {dir:?} to be a created directory");
    }
    // run.json is a file create_run never writes -- only the directories
    // that will later hold it and other artifacts.
    assert!(!paths.run_json().exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn create_run_returns_paths_matching_run_paths_new() {
    let rt = test_runtime();
    let root = unique_store_root("create-run-matches-run-paths");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();

    let created = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");
    let expected = admissionlab_core::RunPaths::new(&root, &run_id);

    assert_eq!(created, expected);

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// Permissions (brief Step 2)
// ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn raw_and_kubeconfigs_directories_are_exactly_mode_0700() {
    let rt = test_runtime();
    let root = unique_store_root("dir-mode-0700");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();

    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    // Asserting the exact mode (not merely "owner-only-ish") is what
    // proves the store sets permissions explicitly after creation
    // rather than relying on the create call's mode argument, which the
    // process umask would otherwise mask down from whatever is
    // requested.
    assert_eq!(mode_of(paths.raw()), 0o700, "raw/ must be exactly 0700");
    assert_eq!(
        mode_of(paths.kubeconfigs()),
        0o700,
        "kubeconfigs/ must be exactly 0700"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn kubeconfig_bearing_file_is_mode_0600() {
    let rt = test_runtime();
    let root = unique_store_root("kubeconfig-file-mode");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();

    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");
    let kubeconfig_path = paths.kubeconfigs().join("baseline.kubeconfig");

    rt.block_on(store.write_bytes_atomic(&kubeconfig_path, b"apiVersion: v1\nkind: Config\n"))
        .expect("write_bytes_atomic should succeed");

    assert_eq!(mode_of(&kubeconfig_path), 0o600);

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// write_json_atomic -- successful writes
// ---------------------------------------------------------------------

#[test]
fn write_json_atomic_produces_a_readable_complete_file() {
    let rt = test_runtime();
    let root = unique_store_root("json-round-trip");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    let value = SampleManifest {
        run_id: run_id.as_str().to_string(),
        fixture_count: 42,
    };

    rt.block_on(store.write_json_atomic(paths.run_json(), &value))
        .expect("write_json_atomic should succeed");

    let contents = std::fs::read_to_string(paths.run_json()).expect("read run.json");
    let read_back: SampleManifest =
        serde_json::from_str(&contents).expect("run.json should contain valid JSON");
    assert_eq!(read_back, value);

    // The run root must contain exactly the five directories create_run
    // makes plus run.json now -- nothing else, and in particular no
    // leftover temporary file from the write that just succeeded.
    assert_eq!(
        sorted_entry_names(paths.root()),
        vec![
            "kubeconfigs",
            "logs",
            "normalized",
            "raw",
            "reports",
            "run.json",
        ]
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn write_bytes_atomic_round_trips_exact_bytes_including_non_utf8() {
    let rt = test_runtime();
    let root = unique_store_root("bytes-round-trip");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    let target = paths.raw().join("capture.bin");
    // 0xC3 0x28 is an invalid two-byte UTF-8 sequence; the rest exercises
    // NUL bytes and the full high-bit byte range.
    let bytes: Vec<u8> = vec![0x00, 0xFF, 0x80, 0x01, 0xFE, b'\n', 0x00, 0xC3, 0x28];
    assert!(
        std::str::from_utf8(&bytes).is_err(),
        "test fixture must actually be invalid UTF-8"
    );

    rt.block_on(store.write_bytes_atomic(&target, &bytes))
        .expect("write_bytes_atomic should succeed");

    let read_back = std::fs::read(&target).expect("read written file");
    assert_eq!(read_back, bytes);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn write_bytes_atomic_fully_replaces_previous_longer_content() {
    let rt = test_runtime();
    let root = unique_store_root("overwrite-truncates");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    let target = paths.reports().join("summary.txt");
    rt.block_on(store.write_bytes_atomic(&target, b"a very long previous report body"))
        .expect("first write should succeed");
    rt.block_on(store.write_bytes_atomic(&target, b"short"))
        .expect("second write should succeed");

    let read_back = std::fs::read(&target).expect("read written file");
    assert_eq!(read_back, b"short");

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// Atomicity under failure (brief Step 3)
// ---------------------------------------------------------------------

#[test]
fn mid_serialization_failure_leaves_no_partial_file_and_no_stray_temp_file() {
    let rt = test_runtime();
    let root = unique_store_root("serialize-failure");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    let result =
        rt.block_on(store.write_json_atomic(paths.run_json(), &FailsPartwayThroughSerialization));

    assert!(
        matches!(result, Err(ArtifactError::Serialize(_))),
        "expected a Serialize error, got {result:?}"
    );
    assert!(
        !paths.run_json().exists(),
        "destination must not exist after a serialization failure"
    );
    // No stray `.run.json.tmp-*` (or any other unexpected) entry: the
    // run root must contain exactly the five directories create_run
    // made, nothing more.
    assert_eq!(
        sorted_entry_names(paths.root()),
        vec!["kubeconfigs", "logs", "normalized", "raw", "reports"]
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mid_serialization_failure_leaves_a_pre_existing_destination_unchanged() {
    let rt = test_runtime();
    let root = unique_store_root("serialize-failure-preexisting");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    std::fs::write(paths.run_json(), b"{\"previous\":true}").expect("seed previous run.json");

    let result =
        rt.block_on(store.write_json_atomic(paths.run_json(), &FailsPartwayThroughSerialization));

    assert!(matches!(result, Err(ArtifactError::Serialize(_))));
    let contents = std::fs::read(paths.run_json()).expect("read run.json");
    assert_eq!(contents, b"{\"previous\":true}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn failed_rename_does_not_leave_a_stray_temp_file() {
    let rt = test_runtime();
    let root = unique_store_root("rename-failure-cleanup");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    let paths = rt
        .block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    // Force the final rename to fail by making the destination an
    // existing directory rather than a file: the OS rejects renaming a
    // regular file onto an existing directory. This exercises the
    // cleanup-on-error path directly, independent of
    // write_json_atomic's serialize-before-any-IO shortcut above.
    let target = paths.reports().join("not-actually-a-file");
    std::fs::create_dir(&target).expect("create blocking directory");

    let result = rt.block_on(store.write_bytes_atomic(&target, b"payload"));
    assert!(
        result.is_err(),
        "rename onto an existing directory must fail"
    );

    assert_eq!(
        sorted_entry_names(paths.reports()),
        vec!["not-actually-a-file"],
        "expected only the pre-existing blocking directory, no leftover temp file"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// Path safety (brief Step 1)
// ---------------------------------------------------------------------

#[test]
fn write_bytes_atomic_rejects_a_path_that_escapes_the_store_root() {
    let rt = test_runtime();
    let root = unique_store_root("path-escapes-root");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    // Ensure the store root exists on disk (create_run creates it as an
    // ancestor of the run root) before probing an escaping write.
    rt.block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    // Textually still starts with `root` component-by-component, but
    // once the `..` is actually followed it resolves to root's own
    // parent -- outside the store entirely. A naive, non-canonicalizing
    // `starts_with` check would be fooled by this; canonicalizing first
    // must not be.
    let escaping = root.join("..").join("escaped.json");

    let result = rt.block_on(store.write_bytes_atomic(&escaping, b"payload"));

    assert!(
        matches!(result, Err(ArtifactError::PathEscapesRoot { .. })),
        "expected PathEscapesRoot, got {result:?}"
    );
    assert!(!escaping.exists(), "escaping write must not have happened");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn write_json_atomic_also_rejects_a_path_that_escapes_the_store_root() {
    let rt = test_runtime();
    let root = unique_store_root("path-escapes-root-json");
    let store = ArtifactStore::new(&root);
    let run_id = RunId::generate();
    rt.block_on(store.create_run(&run_id))
        .expect("create_run should succeed");

    let escaping = root.join("..").join("escaped.json");
    let value = SampleManifest {
        run_id: run_id.as_str().to_string(),
        fixture_count: 1,
    };

    let result = rt.block_on(store.write_json_atomic(&escaping, &value));

    assert!(matches!(result, Err(ArtifactError::PathEscapesRoot { .. })));
    assert!(!escaping.exists());

    let _ = std::fs::remove_dir_all(&root);
}
