//! Unit tests for `admissionlab_installer`'s raw-manifest installation
//! backend (`admissionlab_installer::{ManifestsInstaller,
//! load_manifest_bundle}`, Task 2.3).
//!
//! No test here executes a real `kubectl`, `helm`, `kind`, or `docker`:
//! every test that touches the cluster-facing side drives
//! [`ManifestsInstaller`] through [`FakeProcessRunner`], a
//! [`ProcessRunner`] that never spawns a real process (mirroring
//! `tests/helm_unit.rs`). [`load_manifest_bundle`] itself takes no
//! `ProcessRunner` at all -- its own tests below write real (small,
//! temporary) manifest files to disk and parse them directly, since
//! plain filesystem IO is not an external command.
//!
//! Covers Task 2.3 brief:
//! - Step 1 (multi-document YAML, JSON, deterministic order, duplicate
//!   source files) -- the `load_manifest_bundle`-only tests in the first
//!   section below.
//! - Step 2 (parse locally before invoking cluster operations) --
//!   `malformed_yaml_document_fails_with_file_and_document_identified`
//!   (the pure-parse layer) and
//!   `malformed_manifest_fails_before_any_kubectl_call` (proving the
//!   same property end-to-end through `ManifestsInstaller::install`,
//!   with zero kubectl invocations).
//! - Step 3 (`kubectl apply --server-side=false -f <file>`, always with
//!   `--kubeconfig`) -- the `ManifestsInstaller` tests in the second
//!   section, including
//!   `every_kubectl_invocation_carries_kubeconfig_pointing_at_the_clusters_own_path`
//!   (the regression-proof structural property) and
//!   `annotation_size_limit_failure_is_surfaced_with_clear_diagnostic_and_no_silent_retry`
//!   (the `--server-side=false` annotation-size failure mode, made
//!   legible without a silent `--server-side=true` retry).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use admissionlab_core::{
    ClusterHandle, ClusterSpec, CommandResult, CommandSpec, ProcessError, ProcessRunner, Side,
};
use admissionlab_installer::{
    ComponentInstaller, InstallError, ManifestsInstaller, load_manifest_bundle,
};
use admissionlab_spec::component::HelmInstallSpec;
use admissionlab_spec::{InstallMethod, ManifestInstallSpec, ResolvedComponent};
use async_trait::async_trait;

// ---------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------

/// Builds a normal-exit [`ExitStatus`] reporting `code`, using the
/// well-known Unix `wait(2)` encoding, mirroring `tests/helm_unit.rs`'s
/// own `exit_status` helper.
fn exit_status(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

/// A fresh, guaranteed-unique directory under the system temp directory,
/// mirroring `admissionlab-spec/tests/component.rs`'s own
/// `unique_temp_dir` helper (each integration test binary is compiled
/// separately, so nothing is actually shared between them).
fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-installer-manifests-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}

/// Writes `contents` to `dir.join(name)` and returns the resulting path.
fn write_manifest(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write temp manifest file");
    path
}

/// A [`ClusterHandle`] whose kubeconfig is `kubeconfig` -- deliberately
/// distinctive in every test so a test can prove this exact path (and
/// not some other, ambient one) was passed to `kubectl`.
fn cluster_handle(kubeconfig: &str) -> ClusterHandle {
    ClusterHandle {
        spec: ClusterSpec {
            side: Side::Baseline,
            name: "adlab-baseline-testcluster01".to_owned(),
            kubernetes_version: "1.36.4".to_owned(),
            node_image: "kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed".to_owned(),
        },
        kubeconfig: PathBuf::from(kubeconfig),
        audit_log: PathBuf::from("/run/adlab/baseline-audit.log"),
    }
}

/// Wraps `paths` in a [`ResolvedComponent`] whose install method is
/// `Manifests`.
fn manifests_component(paths: Vec<PathBuf>) -> ResolvedComponent {
    ResolvedComponent {
        name: "raw-webhook".to_owned(),
        version: "1.0.0".to_owned(),
        install: InstallMethod::Manifests(ManifestInstallSpec { paths }),
        readiness: Vec::new(),
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    }
}

/// A resolved component whose install method is `Helm`, not `Manifests`
/// -- used to prove [`ManifestsInstaller`] rejects it up front.
fn helm_component() -> ResolvedComponent {
    ResolvedComponent {
        name: "ingress-nginx".to_owned(),
        version: "4.11.3".to_owned(),
        install: InstallMethod::Helm(HelmInstallSpec {
            repo_name: "ingress-nginx".to_owned(),
            repo_url: "https://kubernetes.github.io/ingress-nginx".to_owned(),
            chart: "ingress-nginx/ingress-nginx".to_owned(),
            version: "4.11.3".to_owned(),
            release_name: "ingress-nginx".to_owned(),
            namespace: "ingress-nginx".to_owned(),
            values_files: Vec::new(),
            set_values: BTreeMap::new(),
        }),
        readiness: Vec::new(),
        recipe_normalize_rules: Vec::new(),
        capabilities: BTreeSet::new(),
    }
}

/// One scripted response [`FakeProcessRunner`] gives for a `kubectl`
/// invocation, keyed by the file path following `-f` in its argv.
#[derive(Clone)]
enum FakeOutcome {
    /// The command ran and exited 0 with this stdout.
    Success(&'static [u8]),
    /// The command ran but exited non-zero, with this stderr.
    Failure(&'static [u8]),
    /// The command could not be spawned at all (simulates `kubectl` not
    /// being on `PATH`).
    Missing,
}

/// Scans `args` for `flag` and returns the single value that follows it.
fn find_flag<'a>(args: &'a [OsString], flag: &str) -> Option<&'a OsString> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
}

/// Identifies which manifest file a `kubectl apply` [`CommandSpec`]
/// targets, by its `-f` flag's value.
fn apply_target_key(args: &[OsString]) -> Option<String> {
    find_flag(args, "-f").map(|value| value.to_string_lossy().into_owned())
}

/// A [`ProcessRunner`] that never spawns a real process: it returns a
/// scripted [`FakeOutcome`] keyed by [`apply_target_key`], and records
/// every [`CommandSpec`] it was given so a test can assert on the exact
/// argv [`ManifestsInstaller`] built. Mirrors `tests/helm_unit.rs`'s own
/// `FakeProcessRunner`, keyed by target file instead of by Helm step
/// (this module only ever runs one kind of `kubectl` invocation --
/// `apply` -- so the file being applied is what distinguishes calls).
struct FakeProcessRunner {
    outcomes: BTreeMap<String, FakeOutcome>,
    calls: Mutex<Vec<CommandSpec>>,
}

impl FakeProcessRunner {
    fn new() -> Self {
        Self {
            outcomes: BTreeMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn with(mut self, path: &Path, outcome: FakeOutcome) -> Self {
        self.outcomes
            .insert(path.to_string_lossy().into_owned(), outcome);
        self
    }

    fn calls(&self) -> Vec<CommandSpec> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }
}

#[async_trait]
impl ProcessRunner for FakeProcessRunner {
    async fn run(&self, spec: CommandSpec) -> Result<CommandResult, ProcessError> {
        self.calls
            .lock()
            .expect("calls mutex poisoned")
            .push(spec.clone());

        let outcome = apply_target_key(&spec.args).and_then(|key| self.outcomes.get(&key));
        match outcome {
            Some(FakeOutcome::Success(stdout)) => Ok(CommandResult {
                status: exit_status(0),
                stdout: stdout.to_vec(),
                stderr: Vec::new(),
                elapsed: Duration::from_millis(1),
            }),
            Some(FakeOutcome::Failure(stderr)) => Ok(CommandResult {
                status: exit_status(1),
                stdout: Vec::new(),
                stderr: stderr.to_vec(),
                elapsed: Duration::from_millis(1),
            }),
            Some(FakeOutcome::Missing) | None => Err(ProcessError::Spawn {
                context: Box::new(spec.context()),
                source: io::Error::new(io::ErrorKind::NotFound, "No such file or directory"),
            }),
        }
    }
}

// ---------------------------------------------------------------------
// Step 1: `load_manifest_bundle` -- multi-document YAML, JSON,
// deterministic order, duplicate source files. Pure parsing: no
// `ProcessRunner` involved at all.
// ---------------------------------------------------------------------

#[test]
fn multi_document_yaml_produces_multiple_documents() {
    let dir = unique_temp_dir("multi-doc-yaml");
    let path = write_manifest(
        &dir,
        "stack.yaml",
        "apiVersion: v1\n\
         kind: Namespace\n\
         metadata:\n  name: demo\n\
         ---\n\
         apiVersion: v1\n\
         kind: ConfigMap\n\
         metadata:\n  name: cfg\n  namespace: demo\n\
         data:\n  key: value\n\
         ---\n\
         apiVersion: v1\n\
         kind: Service\n\
         metadata:\n  name: svc\n  namespace: demo\n\
         spec:\n  ports:\n  - port: 80\n",
    );

    let bundle = load_manifest_bundle(&[path]).expect("well-formed multi-document YAML must load");

    assert_eq!(bundle.documents.len(), 3);
    assert_eq!(bundle.documents[0]["kind"].as_str(), Some("Namespace"));
    assert_eq!(bundle.documents[1]["kind"].as_str(), Some("ConfigMap"));
    assert_eq!(bundle.documents[2]["kind"].as_str(), Some("Service"));
}

#[test]
fn json_input_produces_one_document() {
    let dir = unique_temp_dir("json-input");
    let path = write_manifest(
        &dir,
        "cm.json",
        r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"cfg"},"data":{"key":"value"}}"#,
    );

    let bundle = load_manifest_bundle(&[path]).expect("well-formed JSON must load");

    assert_eq!(bundle.documents.len(), 1);
    assert_eq!(bundle.documents[0]["kind"].as_str(), Some("ConfigMap"));
    assert_eq!(
        bundle.documents[0]["metadata"]["name"].as_str(),
        Some("cfg")
    );
}

#[test]
fn json_extension_is_case_insensitive() {
    let dir = unique_temp_dir("json-ext-case");
    let path = write_manifest(&dir, "cm.JSON", r#"{"kind":"ConfigMap"}"#);

    let bundle = load_manifest_bundle(&[path])
        .expect("an uppercase .JSON extension must still parse as JSON");

    assert_eq!(bundle.documents.len(), 1);
    assert_eq!(bundle.documents[0]["kind"].as_str(), Some("ConfigMap"));
}

#[test]
fn deterministic_order_is_preserved_across_repeated_loads_and_not_sorted() {
    let dir = unique_temp_dir("order");
    // Filenames chosen so alphabetical sorting would reverse this order
    // -- proving order is preserved from the caller's list, not
    // re-sorted by filename.
    let path_b = write_manifest(
        &dir,
        "b-second.yaml",
        "apiVersion: v1\nkind: Zebra\nmetadata:\n  name: z\n",
    );
    let path_a = write_manifest(
        &dir,
        "a-first.yaml",
        "apiVersion: v1\nkind: Alpha\nmetadata:\n  name: a\n",
    );

    let paths = vec![path_b, path_a];
    let first = load_manifest_bundle(&paths).expect("load must succeed");
    let second = load_manifest_bundle(&paths).expect("load must succeed");

    assert_eq!(
        first.documents[0]["kind"].as_str(),
        Some("Zebra"),
        "the caller's declared order (b before a) must be preserved, not alphabetically sorted"
    );
    assert_eq!(first.documents[1]["kind"].as_str(), Some("Alpha"));
    assert_eq!(
        first, second,
        "repeated loads of the same inputs must be byte-for-byte identical"
    );
}

#[test]
fn reordering_input_paths_changes_document_order_and_hash() {
    let dir = unique_temp_dir("reorder");
    let path_a = write_manifest(
        &dir,
        "a.yaml",
        "apiVersion: v1\nkind: Alpha\nmetadata:\n  name: a\n",
    );
    let path_b = write_manifest(
        &dir,
        "b.yaml",
        "apiVersion: v1\nkind: Beta\nmetadata:\n  name: b\n",
    );

    let forward =
        load_manifest_bundle(&[path_a.clone(), path_b.clone()]).expect("load must succeed");
    let backward = load_manifest_bundle(&[path_b, path_a]).expect("load must succeed");

    assert_eq!(forward.documents[0]["kind"].as_str(), Some("Alpha"));
    assert_eq!(backward.documents[0]["kind"].as_str(), Some("Beta"));
    assert_ne!(
        forward.source_hash, backward.source_hash,
        "the hash must reflect the declared file order, not just the set of file contents"
    );
}

#[test]
fn duplicate_source_files_are_read_and_counted_once() {
    let dir = unique_temp_dir("dup");
    let path_a = write_manifest(
        &dir,
        "a.yaml",
        "apiVersion: v1\nkind: Alpha\nmetadata:\n  name: a\n",
    );
    let path_b = write_manifest(
        &dir,
        "b.yaml",
        "apiVersion: v1\nkind: Beta\nmetadata:\n  name: b\n",
    );

    let with_dup = load_manifest_bundle(&[path_a.clone(), path_b.clone(), path_a.clone()])
        .expect("load must succeed");
    let without_dup = load_manifest_bundle(&[path_a, path_b]).expect("load must succeed");

    assert_eq!(
        with_dup.documents.len(),
        2,
        "the repeated file must contribute its document only once"
    );
    assert_eq!(
        with_dup, without_dup,
        "a trailing duplicate must have no effect on the bundle at all -- same documents, same hash"
    );
}

#[test]
fn malformed_yaml_document_fails_with_file_and_document_identified() {
    let dir = unique_temp_dir("malformed-yaml");
    // The first document is well-formed; the second uses a literal tab
    // character for indentation, which the YAML spec forbids -- a
    // reliable, implementation-independent parse error.
    let path = write_manifest(
        &dir,
        "broken.yaml",
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: demo\n---\nkind: ConfigMap\n\tname: cfg\n",
    );

    let error = load_manifest_bundle(std::slice::from_ref(&path))
        .expect_err("a malformed second document must fail to load");

    match error {
        InstallError::ManifestParse {
            path: failed_path,
            document_number,
            format,
            ..
        } => {
            assert_eq!(failed_path, path);
            assert_eq!(
                document_number, 2,
                "the first document is well-formed; the second is broken"
            );
            assert_eq!(format, "YAML");
        }
        other => panic!("expected InstallError::ManifestParse, got {other:?}"),
    }
}

#[test]
fn malformed_json_fails_with_file_identified() {
    let dir = unique_temp_dir("malformed-json");
    let path = write_manifest(&dir, "broken.json", "{\"apiVersion\": \"v1\", \"kind\": }");

    let error = load_manifest_bundle(std::slice::from_ref(&path))
        .expect_err("malformed JSON must fail to load");

    match error {
        InstallError::ManifestParse {
            path: failed_path,
            document_number,
            format,
            ..
        } => {
            assert_eq!(failed_path, path);
            assert_eq!(document_number, 1);
            assert_eq!(format, "JSON");
        }
        other => panic!("expected InstallError::ManifestParse, got {other:?}"),
    }
}

#[test]
fn missing_file_surfaces_as_manifest_read_error() {
    let dir = unique_temp_dir("missing");
    let missing = dir.join("does-not-exist.yaml");

    let error = load_manifest_bundle(std::slice::from_ref(&missing))
        .expect_err("a missing file must fail to load");

    match error {
        InstallError::ManifestRead { path, .. } => assert_eq!(path, missing),
        other => panic!("expected InstallError::ManifestRead, got {other:?}"),
    }
}

#[test]
fn directory_path_surfaces_as_manifest_read_error_not_expanded() {
    let dir = unique_temp_dir("dir-as-manifest");
    // `dir` itself is a real directory; treat it as a (bogus) manifest
    // "file" path to prove Task 2.3 does not silently expand it.
    let error = load_manifest_bundle(std::slice::from_ref(&dir))
        .expect_err("a directory must not be read as a file");

    assert!(matches!(error, InstallError::ManifestRead { .. }));
}

#[test]
fn trailing_empty_yaml_document_is_not_counted() {
    let dir = unique_temp_dir("trailing-empty");
    let path = write_manifest(
        &dir,
        "trailing.yaml",
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: demo\n---\n",
    );

    let bundle = load_manifest_bundle(&[path]).expect("a trailing empty document must not fail");

    assert_eq!(
        bundle.documents.len(),
        1,
        "a trailing bare `---` must not produce a spurious empty document"
    );
}

#[test]
fn source_hash_depends_on_content_not_on_the_paths_holding_it() {
    let dir_one = unique_temp_dir("hash-path-one");
    let dir_two = unique_temp_dir("hash-path-two");
    let content = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n";
    let path_one = write_manifest(&dir_one, "config.yaml", content);
    let path_two = write_manifest(&dir_two, "totally-different-name.yaml", content);

    let bundle_one = load_manifest_bundle(&[path_one]).expect("load must succeed");
    let bundle_two = load_manifest_bundle(&[path_two]).expect("load must succeed");

    assert_eq!(
        bundle_one.source_hash, bundle_two.source_hash,
        "identical content at two different absolute paths must hash identically -- the hash \
         proves which content was applied, not which filesystem layout held it"
    );
}

#[test]
fn source_hash_is_lowercase_hex_sha256_and_changes_with_content() {
    let dir = unique_temp_dir("hash-shape");
    let path = write_manifest(
        &dir,
        "a.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n",
    );
    let bundle = load_manifest_bundle(std::slice::from_ref(&path)).expect("load must succeed");

    assert_eq!(
        bundle.source_hash.len(),
        64,
        "SHA-256 hex must be 64 characters"
    );
    assert!(
        bundle
            .source_hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "must be lowercase hex: {:?}",
        bundle.source_hash
    );

    std::fs::write(
        &path,
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: different\n",
    )
    .expect("overwrite temp manifest file");
    let changed = load_manifest_bundle(&[path]).expect("load must succeed");

    assert_ne!(bundle.source_hash, changed.source_hash);
}

// ---------------------------------------------------------------------
// Step 2 + Step 3: `ManifestsInstaller` -- fail-fast local parsing, then
// `kubectl apply --server-side=false -f <file> --kubeconfig <path>`.
// ---------------------------------------------------------------------

#[tokio::test]
async fn apply_uses_exact_argv_with_server_side_false_and_kubeconfig() {
    let dir = unique_temp_dir("apply-argv");
    let path = write_manifest(
        &dir,
        "cm.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n",
    );

    let runner = Arc::new(
        FakeProcessRunner::new().with(&path, FakeOutcome::Success(b"configmap/cfg created\n")),
    );
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![path.clone()]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program, OsString::from("kubectl"));
    assert_eq!(
        calls[0].args,
        vec![
            OsString::from("apply"),
            OsString::from("--server-side=false"),
            OsString::from("-f"),
            path.into_os_string(),
            OsString::from("--kubeconfig"),
            OsString::from("/run/adlab/baseline.kubeconfig"),
        ]
    );
}

#[tokio::test]
async fn every_kubectl_invocation_carries_kubeconfig_pointing_at_the_clusters_own_path() {
    let dir = unique_temp_dir("kubeconfig-everywhere");
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| {
            write_manifest(
                &dir,
                &format!("m{i}.yaml"),
                &format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg{i}\n"),
            )
        })
        .collect();

    let mut runner_builder = FakeProcessRunner::new();
    for path in &paths {
        runner_builder = runner_builder.with(path, FakeOutcome::Success(b""));
    }
    let runner = Arc::new(runner_builder);
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(paths);
    let distinctive_kubeconfig = "/run/adlab/run-9f2c/candidate.kubeconfig";
    let cluster = cluster_handle(distinctive_kubeconfig);

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        assert_eq!(call.program, OsString::from("kubectl"));
        assert_eq!(
            find_flag(&call.args, "--kubeconfig"),
            Some(&OsString::from(distinctive_kubeconfig)),
            "every kubectl invocation must carry --kubeconfig pointing at this cluster's own \
             kubeconfig"
        );
        assert!(
            !call.env.contains_key(&OsString::from("KUBECONFIG")),
            "kubeconfig selection must go through --kubeconfig alone, never a KUBECONFIG env \
             override"
        );
    }
}

#[tokio::test]
async fn files_are_applied_in_declared_order_one_kubectl_call_per_file() {
    let dir = unique_temp_dir("order-apply");
    let path_first = write_manifest(
        &dir,
        "01-namespace.yaml",
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: demo\n",
    );
    let path_second = write_manifest(
        &dir,
        "02-configmap.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n  namespace: demo\n",
    );

    let runner = Arc::new(
        FakeProcessRunner::new()
            .with(&path_first, FakeOutcome::Success(b""))
            .with(&path_second, FakeOutcome::Success(b"")),
    );
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![path_first.clone(), path_second.clone()]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        find_flag(&calls[0].args, "-f"),
        Some(&path_first.into_os_string())
    );
    assert_eq!(
        find_flag(&calls[1].args, "-f"),
        Some(&path_second.into_os_string())
    );
}

#[tokio::test]
async fn duplicate_paths_are_applied_only_once() {
    let dir = unique_temp_dir("dup-apply");
    let path = write_manifest(
        &dir,
        "cm.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n",
    );

    let runner = Arc::new(FakeProcessRunner::new().with(&path, FakeOutcome::Success(b"")));
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![path.clone(), path]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    assert_eq!(
        runner.calls().len(),
        1,
        "a repeated path must be applied only once"
    );
}

#[tokio::test]
async fn malformed_manifest_fails_before_any_kubectl_call() {
    let dir = unique_temp_dir("fail-fast");
    let good = write_manifest(
        &dir,
        "good.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n",
    );
    let bad = write_manifest(&dir, "bad.yaml", "kind: ConfigMap\n\tname: cfg\n");

    let runner = Arc::new(FakeProcessRunner::new().with(&good, FakeOutcome::Success(b"")));
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![good, bad]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a malformed manifest must fail the install");

    assert!(matches!(error, InstallError::ManifestParse { .. }));
    assert!(
        runner.calls().is_empty(),
        "no kubectl invocation may run until every manifest in the component has parsed"
    );
}

#[tokio::test]
async fn nonzero_kubectl_exit_surfaces_as_install_error_with_stderr_not_panic() {
    let dir = unique_temp_dir("nonzero");
    let path_first = write_manifest(
        &dir,
        "01.yaml",
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: demo\n",
    );
    let path_second = write_manifest(
        &dir,
        "02.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n  namespace: nope\n",
    );

    let runner = Arc::new(
        FakeProcessRunner::new()
            .with(&path_first, FakeOutcome::Success(b""))
            .with(
                &path_second,
                FakeOutcome::Failure(
                    b"Error from server (NotFound): namespaces \"nope\" not found\n",
                ),
            ),
    );
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![path_first, path_second]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a non-zero kubectl exit must surface as an error, not a panic");

    match error {
        InstallError::CommandFailed {
            component: failed_component,
            stderr,
            status,
            ..
        } => {
            assert_eq!(failed_component, "raw-webhook");
            assert!(!status.success());
            assert!(String::from_utf8_lossy(&stderr).contains("NotFound"));
        }
        other => panic!("expected InstallError::CommandFailed, got {other:?}"),
    }
    assert_eq!(
        runner.calls().len(),
        2,
        "the first file's apply and the second file's failed apply -- nothing after the failure"
    );
}

#[tokio::test]
async fn kubectl_not_found_surfaces_as_install_error_process_variant() {
    let dir = unique_temp_dir("missing-kubectl");
    let path = write_manifest(
        &dir,
        "cm.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n",
    );

    let runner = Arc::new(FakeProcessRunner::new().with(&path, FakeOutcome::Missing));
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![path]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a missing kubectl binary must surface as an error");

    match error {
        InstallError::Process { component, source } => {
            assert_eq!(component, "raw-webhook");
            assert!(matches!(source, ProcessError::Spawn { .. }));
        }
        other => panic!("expected InstallError::Process, got {other:?}"),
    }
}

#[tokio::test]
async fn helm_install_method_is_rejected_without_invoking_runner() {
    let runner = Arc::new(FakeProcessRunner::new());
    let installer = ManifestsInstaller::new(runner.clone());
    let component = helm_component();
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a Helm component must be rejected");

    assert!(matches!(error, InstallError::UnsupportedMethod { .. }));
    assert!(
        runner.calls().is_empty(),
        "the manifests installer must not run anything for a non-manifests component"
    );
}

#[tokio::test]
async fn successful_install_reports_declared_version_and_manifests_method() {
    let dir = unique_temp_dir("record");
    let path = write_manifest(
        &dir,
        "cm.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n",
    );

    let runner = Arc::new(FakeProcessRunner::new().with(&path, FakeOutcome::Success(b"")));
    let installer = ManifestsInstaller::new(runner);
    let component = manifests_component(vec![path]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let record = installer
        .install(&cluster, &component)
        .await
        .expect("install should succeed");

    assert_eq!(record.component, "raw-webhook");
    assert_eq!(record.method, "manifests");
    assert_eq!(
        record.resolved_version, "1.0.0",
        "a manifests install has no independent version source to confirm against, so \
         resolved_version is the component's own declared version"
    );
    assert!(record.diagnostics.is_empty());
}

// ---------------------------------------------------------------------
// `--server-side=false`'s known annotation-size failure mode
// ---------------------------------------------------------------------

#[tokio::test]
async fn annotation_size_limit_failure_is_surfaced_with_clear_diagnostic_and_no_silent_retry() {
    let dir = unique_temp_dir("annotation-limit");
    let path = write_manifest(
        &dir,
        "hugecrd.yaml",
        "apiVersion: apiextensions.k8s.io/v1\nkind: CustomResourceDefinition\nmetadata:\n  name: hugecrd.example.io\n",
    );

    let stderr: &[u8] = b"error: CustomResourceDefinition \"hugecrd.example.io\" is invalid: \
                           metadata.annotations: Too long: must have at most 262144 bytes\n";
    let runner = Arc::new(FakeProcessRunner::new().with(&path, FakeOutcome::Failure(stderr)));
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![path.clone()]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("exceeding the annotation limit must fail the install");

    match &error {
        InstallError::ManifestExceedsAnnotationLimit {
            path: failed_path,
            stderr: captured_stderr,
            ..
        } => {
            assert_eq!(failed_path, &path);
            assert_eq!(
                captured_stderr.as_slice(),
                stderr,
                "the original kubectl stderr must be preserved, not discarded"
            );
        }
        other => panic!("expected InstallError::ManifestExceedsAnnotationLimit, got {other:?}"),
    }

    let message = error.to_string();
    assert!(
        message.contains("Helm"),
        "the message must point to a concrete remedy; got {message:?}"
    );
    assert!(
        message.contains("will not silently retry"),
        "the message must make clear no automatic retry happened; got {message:?}"
    );
    assert_eq!(
        runner.calls().len(),
        1,
        "this failure must never trigger an automatic retry with a different flag"
    );
}

#[tokio::test]
async fn plain_nonzero_exit_without_annotation_limit_wording_stays_a_generic_command_failed() {
    let dir = unique_temp_dir("not-annotation-limit");
    let path = write_manifest(
        &dir,
        "cm.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\n",
    );

    let runner = Arc::new(FakeProcessRunner::new().with(
        &path,
        FakeOutcome::Failure(b"Error from server (Forbidden): configmaps is forbidden\n"),
    ));
    let installer = ManifestsInstaller::new(runner.clone());
    let component = manifests_component(vec![path]);
    let cluster = cluster_handle("/run/adlab/baseline.kubeconfig");

    let error = installer
        .install(&cluster, &component)
        .await
        .expect_err("a forbidden response must still fail the install");

    assert!(
        matches!(error, InstallError::CommandFailed { .. }),
        "an unrelated failure must not be misclassified as the annotation-size-limit case; got \
         {error:?}"
    );
}
