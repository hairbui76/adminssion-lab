//! `admissionlab reproduce`, end to end against real clusters (ROADMAP
//! Task 5.3 step 5).
//!
//! ```bash
//! cargo test -p admissionlab-cli --test reproduce_e2e -- --ignored --nocapture
//! ```
//!
//! # What this proves that nothing else can
//!
//! `tests/reproduce_command.rs` proves the *mechanism*: the recorded
//! `image@digest` reaches `ClusterManager::create`, the recorded version
//! reaches the installer, a tampered fixture refuses. Every one of those
//! runs against a fake, and a fake answers what it was told to answer.
//!
//! The claim this file makes is the product's: **reproducing a recorded
//! run twice produces the same semantic result.** That needs four real
//! `kind` clusters across two reproductions of one real recorded run,
//! four real controller stacks, and two real API servers per run
//! disagreeing with each other in exactly the same way both times.
//!
//! # Timestamp and identifier normalization, defined exactly
//!
//! Two runs of the same lab cannot produce byte-identical `result.json`
//! files, and demanding that would be demanding something false: run
//! identifiers differ, cluster names embed them, wall-clock latencies
//! differ, and the API server stamps every admitted object with its own
//! identity and time. [`semantic_signature`] removes exactly those and
//! nothing else, and the two signatures are then compared **byte for
//! byte** — not field by field, so a difference this file did not
//! anticipate fails rather than slipping past an assertion that only
//! looked at what someone thought to check.
//!
//! The definition, in full:
//!
//! 1. Parse `result.json`.
//! 2. Take the top-level `runId`; it is needed in step 4 before it is
//!    removed.
//! 3. Recursively remove every object member named by
//!    [`VOLATILE_KEYS`], at any depth. That set is the union of four
//!    things and is deliberately closed:
//!    - `runId` — the run's own identifier.
//!    - `total_latency`, `latency` — the only two duration-valued fields
//!      the result schema carries
//!      (`admissionlab_admission::AdmissionOutcome::total_latency` and
//!      `WebhookInvocation::latency`), both wall-clock measurements.
//!    - `uid`, `resourceVersion`, `creationTimestamp`, `managedFields` —
//!      the server-assigned identity and timestamps on every admitted
//!      object. These are exactly the four pointers
//!      `admissionlab_normalize::built_in_rules` already removes before
//!      *comparing* two objects; they survive into the report because
//!      `result.json` carries each side's **captured** object, not its
//!      normalized one (see `pipeline::compare`), and they are as
//!      nondeterministic there as they are anywhere.
//!    - `batch.kubernetes.io/controller-uid` and its deprecated
//!      unprefixed twin `controller-uid` — the *same* server-assigned
//!      UUID as `metadata.uid`, wearing a different hat. The API server
//!      defaults a `Job`'s own UID into `spec.selector.matchLabels` and
//!      into `spec.template.metadata.labels`, where it is a label
//!      **value** rather than a `uid` field, so no rule keyed on `uid`
//!      reaches it. The corpus's `policy-job.yaml` is where it shows up,
//!      and it was the only difference between two reproductions before
//!      these two keys were added.
//! 4. Serialize compactly, then replace every occurrence of the run
//!    identifier from step 2 with `<RUN_ID>`, and every occurrence of its
//!    first 12 characters with `<SHORT_RUN_ID>` — the short form
//!    `admissionlab_core::run` embeds in `adlab-<side>-<short>` cluster
//!    names and in every path under the run workspace, both of which
//!    reach the report through diagnostics. The full identifier is
//!    replaced first, so what remains for the short form is a genuine
//!    short-form occurrence.
//!
//! Nothing else is touched. Every semantic change, every severity, every
//! expectation, every bucket count, every captured object field, every
//! webhook invocation, and every first-divergence attribution is compared
//! exactly as written.
//!
//! # Which manifest the second reproduction reads
//!
//! Both reproductions read the **original** run's manifest, not the first
//! reproduction's. That is the stronger and the more useful claim: it
//! says two independent reproductions of one recorded run agree, rather
//! than that a chain of reproductions does not drift — which would pass
//! just as happily if reproduction number one had already lost the
//! recorded environment.
//!
//! # Cleanup
//!
//! The binary deletes its own clusters on every ordinary path. This file
//! verifies that after every run with [`assert_no_leaked_clusters`],
//! best-effort deleting anything it finds first, exactly as
//! `tests/alpha_e2e.rs` does and for the same reason.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

// ---------------------------------------------------------------------
// Locations
// ---------------------------------------------------------------------

/// The workspace root: two levels above this crate's own manifest
/// directory.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The canonical demo's source root — the directory holding its
/// `admissionlab.yaml`, which is exactly what `--source-root` names.
fn example_source_root() -> PathBuf {
    workspace_root().join("examples/kyverno-istio-upgrade")
}

/// The compiled binary this test drives.
fn admissionlab() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_admissionlab"))
}

/// A fresh, guaranteed-unique report directory.
fn report_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-reproduce-e2e-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create the run's report directory");
    dir
}

// ---------------------------------------------------------------------
// The normalization
// ---------------------------------------------------------------------

/// Every object member removed before two results are compared. See this
/// module's "Timestamp and identifier normalization" section for what
/// each one is and why it is in this set.
const VOLATILE_KEYS: &[&str] = &[
    "runId",
    "total_latency",
    "latency",
    "uid",
    "resourceVersion",
    "creationTimestamp",
    "managedFields",
    "batch.kubernetes.io/controller-uid",
    "controller-uid",
];

/// How many leading characters of a run identifier reach a cluster name.
/// Mirrors `admissionlab_core::run`'s own `SHORT_RUN_ID_LEN`, which is
/// private to that crate.
const SHORT_RUN_ID_LEN: usize = 12;

/// One run's `result.json`, reduced to what two runs of the same lab must
/// agree on exactly.
fn semantic_signature(result_text: &str) -> String {
    let mut value: Value =
        serde_json::from_str(result_text).expect("result.json must be valid JSON");
    let run_id = value["runId"]
        .as_str()
        .expect("result.json always carries a runId")
        .to_owned();

    strip_volatile(&mut value);
    let rendered = serde_json::to_string(&value).expect("re-encoding a parsed value cannot fail");

    let short: String = run_id.chars().take(SHORT_RUN_ID_LEN).collect();
    rendered
        .replace(&run_id, "<RUN_ID>")
        .replace(&short, "<SHORT_RUN_ID>")
}

/// Removes every [`VOLATILE_KEYS`] member from `value`, at any depth.
fn strip_volatile(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !VOLATILE_KEYS.contains(&key.as_str()));
            for entry in map.values_mut() {
                strip_volatile(entry);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_volatile(item);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------
// The real thing
// ---------------------------------------------------------------------

#[test]
#[ignore = "requires Docker and kind"]
fn reproducing_the_dogfood_run_twice_produces_the_same_semantic_result() {
    let recorded = run_test("record");
    assert_no_leaked_clusters("after the recorded run");
    let manifest = manifest_path(&recorded);
    assert!(
        manifest.is_file(),
        "the recorded run must have left a manifest at {}",
        manifest.display()
    );

    let first = run_reproduce(&manifest, "reproduce-1", None);
    assert_no_leaked_clusters("after the first reproduction");
    let second = run_reproduce(&manifest, "reproduce-2", None);
    assert_no_leaked_clusters("after the second reproduction");

    // Every reproduction of this lab reaches the same verdict the
    // recorded run did: one unaccounted-for critical change, exit 1.
    for (label, run) in [
        ("the recorded run", &recorded),
        ("the first reproduction", &first),
        ("the second reproduction", &second),
    ] {
        assert_eq!(
            run.exit_code,
            Some(1),
            "{label} must reach the demo's known critical regression"
        );
    }

    // ROADMAP Task 5.3 step 5, byte for byte over everything that is not
    // a run identifier or a wall-clock measurement.
    let first_signature = semantic_signature(&first.result_text);
    let second_signature = semantic_signature(&second.result_text);
    assert_eq!(
        first_signature,
        second_signature,
        "two reproductions of one recorded run disagreed; first difference at byte {}",
        first_difference(&first_signature, &second_signature)
    );

    // A signature that normalized everything away would satisfy the
    // assertion above and prove nothing. This is what makes it mean
    // something: the compared document still carries the demo's actual
    // findings.
    for needle in [
        "init_container_removed",
        "alpha-audit-init",
        "\"fixturesTotal\":10",
        "\"critical\":1",
    ] {
        assert!(
            first_signature.contains(needle),
            "the normalized signature must still contain {needle:?}; normalizing away the \
             findings would make this test vacuous"
        );
    }
    assert!(
        !first_signature.contains(&recorded_run_id(&first)),
        "no run identifier may survive normalization"
    );

    // Both reproductions ran the environment the recorded manifest named,
    // not whatever resolves today.
    for run in [&first, &second] {
        assert!(
            run.stdout
                .contains("Reproducing with the recorded environment:"),
            "a reproduction must say what it pinned before it provisions anything:\n{}",
            run.stdout
        );
    }
    assert_eq!(
        recorded_environments(&recorded),
        recorded_environments(&first),
        "a reproduction must report the same Kubernetes versions and component versions the \
         recorded run did"
    );
}

/// ROADMAP Task 5.3 step 1, against the real corpus: one flipped byte in
/// one fixture and the command refuses.
///
/// Deliberately **not** `#[ignore]`d, and that is the point of it. A
/// refusal that costs a Docker daemon would not be a refusal "before any
/// cluster is created" — so this runs in `cargo test --workspace`, on a
/// machine with no Docker at all, over the real ten-fixture corpus. It
/// needs nothing from the recorded run above: it builds its own manifest
/// from the example's current hashes, which is exactly the manifest a
/// faithful run would have written a moment ago.
#[test]
fn a_single_flipped_byte_in_a_real_fixture_refuses_the_reproduction() {
    let source = example_source_root();
    // The version-aware loader, because the examples are `v1beta1`
    // documents as of Task 7.7 and because it is what the binary uses.
    let lab = admissionlab_spec::load_any_supported_lab(&source.join("admissionlab.yaml"))
        .expect("the canonical example must load and resolve");
    let fixtures =
        admissionlab_fixtures::discover_fixtures(&lab.fixtures).expect("discover the corpus");

    // A manifest recording the corpus exactly as it stands right now.
    let dir = report_dir("tamper");
    let manifest_path = dir.join("run.json");
    let mut manifest = serde_json::json!({
        "schemaVersion": admissionlab_core::run_manifest::SCHEMA_VERSION,
        "runId": "00000000-0000-4000-8000-000000000000",
        "admissionlabVersion": "1.0.0-rc.1",
        "status": "completed",
        "stage": "completed",
        "host": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH},
        "tools": {"kind": null, "kubectl": null, "helm": null, "docker": null},
        "baseline": environment_json(&lab.baseline),
        "candidate": environment_json(&lab.candidate),
        "configSha256": admissionlab_core::file_sha256(&source.join("admissionlab.yaml"))
            .expect("hash the lab file"),
        "fixtureHashes": {},
        "expectationsSha256": lab.expectations_file.as_ref().map(|path| {
            admissionlab_core::file_sha256(path).expect("hash the expectations file")
        }),
        "normalizationSha256": "0".repeat(64),
        "policySha256": "0".repeat(64),
        "startedAt": "2026-09-01T00:00:00.000000000Z",
        "completedAt": "2026-09-01T00:00:00.000000000Z",
    });
    let hashes = manifest["fixtureHashes"]
        .as_object_mut()
        .expect("just built as an object");
    for fixture in &fixtures {
        hashes.insert(
            fixture.id.as_str().to_owned(),
            Value::String(fixture.sha256.clone()),
        );
    }
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).expect("serialize"),
    )
    .expect("write the manifest");

    // Flip one byte of one fixture, run, and put it back whatever
    // happens — this edits a checked-in file, so the restore must not
    // depend on the assertions passing.
    let victim = source.join("fixtures/policy-pod-image.yaml");
    let original = std::fs::read(&victim).expect("read the fixture");
    let mut tampered = original.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 0x01;
    std::fs::write(&victim, &tampered).expect("tamper with the fixture");

    let output = Command::new(admissionlab())
        .arg("reproduce")
        .arg(&manifest_path)
        .arg("--source-root")
        .arg(&source)
        .output();
    std::fs::write(&victim, &original).expect("restore the fixture");
    let output = output.expect("failed to execute the admissionlab binary");

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(2),
        "a tampered fixture is invalid input for a reproduction; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("no longer matches the recorded run"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("policy-pod-image.yaml"),
        "the refusal must name the file that changed; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("expected sha256") && stderr.contains("actual   sha256"),
        "the refusal must carry both digests; stderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// One environment, in the shape a run manifest records it.
///
/// The node image is left unpinned deliberately: this manifest exists to
/// be *refused*, and it is refused at the digest check, long before
/// anything would look at an image.
fn environment_json(environment: &admissionlab_spec::ResolvedEnvironment) -> Value {
    serde_json::json!({
        "kubernetesVersion": environment.kubernetes,
        "nodeImage": format!("kindest/node:v{}", environment.kubernetes),
        "nodeImageDigest": null,
        "components": environment.components.iter().map(|component| serde_json::json!({
            "name": component.name,
            "version": component.version,
            "sourceSha256": null,
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------
// Driving the binary
// ---------------------------------------------------------------------

/// One completed invocation's observable output.
struct LabRun {
    exit_code: Option<i32>,
    stdout: String,
    result: Value,
    result_text: String,
}

/// Runs `admissionlab test` on the canonical example.
fn run_test(label: &str) -> LabRun {
    invoke(
        label,
        &[
            "test".as_ref(),
            example_source_root().join("admissionlab.yaml").as_os_str(),
        ],
        label,
    )
}

/// Runs `admissionlab reproduce` against `manifest`.
fn run_reproduce(manifest: &Path, label: &str, config: Option<&Path>) -> LabRun {
    let source = example_source_root();
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "reproduce".as_ref(),
        manifest.as_os_str(),
        "--source-root".as_ref(),
        source.as_os_str(),
    ];
    if let Some(config) = config {
        args.push("--config".as_ref());
        args.push(config.as_os_str());
    }
    invoke(label, &args, label)
}

/// Runs the binary with `args` plus `--report-dir`, and reads back only
/// what a user can read.
fn invoke(label: &str, args: &[&std::ffi::OsStr], report_label: &str) -> LabRun {
    let reports = report_dir(report_label);
    let binary = admissionlab();

    eprintln!("reproduce_e2e[{label}]: running {} ...", binary.display());
    let started = std::time::Instant::now();
    let output = Command::new(&binary)
        .args(args)
        .arg("--report-dir")
        .arg(&reports)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to execute the admissionlab binary");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    eprintln!(
        "reproduce_e2e[{label}]: exit {:?} in {:.1}s",
        output.status.code(),
        elapsed.as_secs_f64()
    );
    eprintln!("{stdout}");
    if !stderr.is_empty() {
        eprintln!("--- stderr ---\n{stderr}");
    }

    let result_path = reports.join("result.json");
    assert!(
        result_path.is_file(),
        "[{label}] wrote no result.json to {}; exit {:?}\nstderr: {stderr}",
        reports.display(),
        output.status.code(),
    );
    let result_text = std::fs::read_to_string(&result_path).expect("read result.json");
    let result: Value = serde_json::from_str(&result_text).expect("result.json must be valid JSON");

    LabRun {
        exit_code: output.status.code(),
        stdout,
        result,
        result_text,
    }
}

/// Where the run whose `result.json` this is left its run manifest.
///
/// `--report-dir` moves `result.json`/`report.html`; `run.json` always
/// stays in the run's own workspace under the run root, which is why
/// this is derived from the run identifier rather than looked for beside
/// the report. `default_run_root` is the binary's own function, so this
/// cannot drift from where the binary actually writes.
fn manifest_path(run: &LabRun) -> PathBuf {
    admissionlab_cli::commands::test::default_run_root()
        .join(recorded_run_id(run))
        .join("run.json")
}

/// One run's identifier, as its own report states it.
fn recorded_run_id(run: &LabRun) -> String {
    run.result["runId"]
        .as_str()
        .expect("result.json always carries a runId")
        .to_owned()
}

/// One run's reported environments, reduced to the versions a
/// reproduction is required to reuse.
fn recorded_environments(run: &LabRun) -> BTreeSet<String> {
    ["baseline", "candidate"]
        .into_iter()
        .flat_map(|side| {
            let environment = &run.result["environments"][side];
            let kubernetes = format!("{side}|kubernetes|{}", environment["kubernetes"]);
            let components = environment["components"]
                .as_array()
                .expect("every environment reports its components")
                .iter()
                .map(move |component| {
                    format!("{side}|{}|{}", component["name"], component["version"])
                })
                .collect::<Vec<_>>();
            std::iter::once(kubernetes).chain(components)
        })
        .collect()
}

/// A short excerpt around the first byte at which two signatures differ,
/// so a failure reads as a difference rather than as two walls of JSON.
fn first_difference(left: &str, right: &str) -> String {
    let at = left
        .bytes()
        .zip(right.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| left.len().min(right.len()));
    let from = at.saturating_sub(120);
    let excerpt = |text: &str| {
        text.get(from..(at + 120).min(text.len()))
            .unwrap_or("<not a character boundary>")
            .to_owned()
    };
    format!(
        "{at}\n--- first ---\n{}\n--- second ---\n{}",
        excerpt(left),
        excerpt(right)
    )
}

// ---------------------------------------------------------------------
// Cleanup verification
// ---------------------------------------------------------------------

/// Fails if any `adlab-*` cluster survived, after best-effort deleting
/// whatever it found. Identical in behavior and reasoning to
/// `tests/alpha_e2e.rs`'s own guard.
fn assert_no_leaked_clusters(context: &str) {
    let output = Command::new("kind")
        .arg("get")
        .arg("clusters")
        .output()
        .expect("failed to run `kind get clusters`");
    assert!(
        output.status.success(),
        "`kind get clusters` failed {context}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let leaked: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with("adlab-"))
        .map(str::to_owned)
        .collect();
    for name in &leaked {
        let _ = Command::new("kind")
            .args(["delete", "cluster", "--name", name])
            .status();
    }
    assert!(
        leaked.is_empty(),
        "clusters left behind {context}: {leaked:?} (deleted by this test, but the binary \
         should have deleted them itself)"
    );
}
