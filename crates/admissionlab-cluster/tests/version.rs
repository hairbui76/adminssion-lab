//! Behavioral tests for the Kubernetes-version-to-`kind`-node-image
//! matrix ([`admissionlab_cluster::version`]).
//!
//! Two properties matter most and are each covered below:
//!
//! - **Exact-match and unsupported-minor behavior** (brief Step 2):
//!   `resolve_node_image` matches `requested` exactly against either a
//!   known [`KubernetesImage::version`] or [`KubernetesImage::minor`] —
//!   never a semver range or "closest available patch" — and refuses an
//!   entry whose `supported` flag is `false` with a distinct, more
//!   specific error than "never heard of this version."
//! - **The real checked-in `compatibility/kubernetes.yaml` is exactly
//!   right**: `load_matrix` parses it successfully and it pins the
//!   exact versions/digests this task's dispatch specified — a typo
//!   here would otherwise silently pin the wrong `kind` node image, with
//!   nothing else in the type system able to catch it.

use admissionlab_cluster::{
    KubernetesImage, KubernetesImageMatrix, VersionError, load_matrix, resolve_node_image,
};

// ---------------------------------------------------------------------
// Test support: a small, independent matrix — deliberately different
// from the real checked-in file, so these tests exercise
// `resolve_node_image`'s logic in isolation from that file's content.
// ---------------------------------------------------------------------

fn sample_matrix() -> KubernetesImageMatrix {
    KubernetesImageMatrix {
        releases: vec![
            KubernetesImage {
                minor: "1.36".to_string(),
                version: "1.36.4".to_string(),
                image: "kindest/node:v1.36.4".to_string(),
                digest: "sha256:aaaa".to_string(),
                supported: true,
            },
            KubernetesImage {
                minor: "1.34".to_string(),
                version: "1.34.11".to_string(),
                image: "kindest/node:v1.34.11".to_string(),
                digest: "sha256:bbbb".to_string(),
                supported: false,
            },
        ],
    }
}

// ---------------------------------------------------------------------
// resolve_node_image: exact-match behavior
// ---------------------------------------------------------------------

#[test]
fn exact_full_version_match_resolves() {
    let resolved = resolve_node_image("1.36.4", &sample_matrix()).expect("must resolve");
    assert_eq!(resolved.version, "1.36.4");
    assert_eq!(resolved.pinned_image, "kindest/node:v1.36.4@sha256:aaaa");
}

#[test]
fn bare_minor_resolves_to_its_pinned_patch() {
    let resolved = resolve_node_image("1.36", &sample_matrix()).expect("must resolve");
    assert_eq!(resolved.version, "1.36.4");
    assert_eq!(resolved.pinned_image, "kindest/node:v1.36.4@sha256:aaaa");
}

#[test]
fn mismatched_patch_within_a_known_minor_is_unknown() {
    // "1.36.9" is not itself a pinned `version`, and is not equal to any
    // `minor` field either: this is an exact match, not "closest
    // available patch for this minor."
    let err = resolve_node_image("1.36.9", &sample_matrix()).unwrap_err();
    assert!(matches!(err, VersionError::UnknownVersion { .. }));
}

#[test]
fn resolution_is_case_sensitive_and_exact() {
    // No normalization: a "v"-prefixed or otherwise-decorated version
    // string is simply unknown, not fuzzily accepted.
    let err = resolve_node_image("v1.36.4", &sample_matrix()).unwrap_err();
    assert!(matches!(err, VersionError::UnknownVersion { .. }));
}

// ---------------------------------------------------------------------
// resolve_node_image: unsupported minor
// ---------------------------------------------------------------------

#[test]
fn unsupported_minor_is_rejected_with_a_specific_error() {
    let err = resolve_node_image("1.34.11", &sample_matrix()).unwrap_err();
    match err {
        VersionError::UnsupportedMinor {
            requested,
            minor,
            version,
        } => {
            assert_eq!(requested, "1.34.11");
            assert_eq!(minor, "1.34");
            assert_eq!(version, "1.34.11");
        }
        other => panic!("expected UnsupportedMinor, got {other:?}"),
    }
}

#[test]
fn unsupported_minor_by_bare_minor_request_is_also_rejected() {
    let err = resolve_node_image("1.34", &sample_matrix()).unwrap_err();
    assert!(matches!(err, VersionError::UnsupportedMinor { .. }));
}

#[test]
fn unknown_version_is_distinct_from_unsupported_minor() {
    // A version the matrix has genuinely never heard of (as opposed to
    // one it explicitly retired) is a different, more generic error —
    // callers can tell "typo / never valid" apart from "was valid, is
    // now retired."
    let err = resolve_node_image("1.20.0", &sample_matrix()).unwrap_err();
    assert!(matches!(err, VersionError::UnknownVersion { .. }));
    assert!(!matches!(err, VersionError::UnsupportedMinor { .. }));
}

#[test]
fn empty_matrix_rejects_everything() {
    let empty = KubernetesImageMatrix {
        releases: Vec::new(),
    };
    let err = resolve_node_image("1.36.4", &empty).unwrap_err();
    assert!(matches!(err, VersionError::UnknownVersion { .. }));
}

#[test]
fn error_messages_name_the_requested_value() {
    let unknown = resolve_node_image("1.20.0", &sample_matrix()).unwrap_err();
    assert!(unknown.to_string().contains("1.20.0"));

    let unsupported = resolve_node_image("1.34.11", &sample_matrix()).unwrap_err();
    let message = unsupported.to_string();
    assert!(message.contains("1.34"));
}

// ---------------------------------------------------------------------
// load_matrix: the real checked-in compatibility/kubernetes.yaml
// ---------------------------------------------------------------------

#[test]
fn checked_in_matrix_loads() {
    let matrix = load_matrix().expect("compatibility/kubernetes.yaml must parse");
    assert!(!matrix.releases.is_empty());
}

#[test]
fn checked_in_matrix_has_exactly_the_specified_minors_and_digests() {
    let matrix = load_matrix().expect("compatibility/kubernetes.yaml must parse");

    let by_minor = |minor: &str| {
        matrix
            .releases
            .iter()
            .find(|r| r.minor == minor)
            .unwrap_or_else(|| panic!("matrix must contain minor {minor:?}; got {matrix:?}"))
    };

    // Exactly the digests specified for kind v0.33.0's node images.
    let supported_expectations = [
        (
            "1.37",
            "1.37.0",
            "kindest/node:v1.37.0",
            "sha256:a1ed56cfb0e7b93589bdf97c8cd566405a265939e3620fc4f5de89adff580ae5",
        ),
        (
            "1.36",
            "1.36.4",
            "kindest/node:v1.36.4",
            "sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed",
        ),
        (
            "1.35",
            "1.35.8",
            "kindest/node:v1.35.8",
            "sha256:07b2536e30b803ed61d1677a79df6115f798ce64c80f9e22f6ed45afd09323c0",
        ),
    ];
    for (minor, version, image, digest) in supported_expectations {
        let entry = by_minor(minor);
        assert_eq!(entry.version, version, "minor {minor} version");
        assert_eq!(entry.image, image, "minor {minor} image");
        assert_eq!(entry.digest, digest, "minor {minor} digest");
        assert!(entry.supported, "minor {minor} must be marked supported");
    }

    let retired = by_minor("1.34");
    assert_eq!(retired.version, "1.34.11");
    assert_eq!(retired.image, "kindest/node:v1.34.11");
    assert_eq!(
        retired.digest,
        "sha256:44e222ee2132dab25ff87301682f89eb82c7880ea3a1bf543bfe9708fd08d67d"
    );
    assert!(
        !retired.supported,
        "minor 1.34 must be explicitly marked unsupported, not silently omitted"
    );

    assert_eq!(
        matrix.releases.len(),
        4,
        "expected exactly 1.37, 1.36, 1.35 (supported) and 1.34 (retired); got {matrix:?}"
    );
}

#[test]
fn checked_in_matrix_resolves_its_primary_supported_version() {
    // End-to-end: the real file, through the real resolver, for the
    // Tier-1 primary supported version this task's dispatch designated.
    let matrix = load_matrix().unwrap();
    let resolved = resolve_node_image("1.36.4", &matrix).expect("1.36.4 must resolve");
    assert_eq!(
        resolved.pinned_image,
        "kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed"
    );
}

#[test]
fn checked_in_matrix_resolves_all_three_supported_minors_by_bare_minor() {
    let matrix = load_matrix().unwrap();
    for minor in ["1.35", "1.36", "1.37"] {
        resolve_node_image(minor, &matrix)
            .unwrap_or_else(|e| panic!("minor {minor} must resolve: {e}"));
    }
}

#[test]
fn checked_in_matrix_rejects_the_retired_minor_through_the_real_resolver() {
    let matrix = load_matrix().unwrap();
    let err = resolve_node_image("1.34.11", &matrix).unwrap_err();
    assert!(matches!(err, VersionError::UnsupportedMinor { .. }));
}

#[test]
fn checked_in_matrix_rejects_an_eol_version_never_in_the_matrix() {
    // 1.33 is already fully EOL and was never added to the matrix at
    // all (unlike 1.34, which is retained with `supported: false`).
    let matrix = load_matrix().unwrap();
    let err = resolve_node_image("1.33.13", &matrix).unwrap_err();
    assert!(matches!(err, VersionError::UnknownVersion { .. }));
}
