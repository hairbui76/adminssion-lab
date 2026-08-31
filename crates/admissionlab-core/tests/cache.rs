//! Behavioral tests for the immutable content cache (Task 5.6).
//!
//! Three groups:
//!
//! - **Where the cache root is** — the `ADMISSIONLAB_CACHE_DIR` override
//!   and its absoluteness rule, driven through
//!   [`CachePaths::resolve_with`] rather than the real environment
//!   variable, because setting one is `unsafe` in Rust 2024 and this
//!   workspace forbids `unsafe_code`. (The platform-default rules
//!   themselves are unit-tested inside `src/cache.rs`, where the pure
//!   function that decides them lives.)
//! - **What may be cached** — the containment guarantee, driven by
//!   feeding [`ContentKey::new`] every shape of name and digest that
//!   could otherwise escape the cache root. The *other* half of Task 5.6
//!   Step 3 — that no cache method takes a run identifier — is enforced
//!   by the API's own signatures and so is a compile-time property, not
//!   something a test can assert at runtime; `cache.rs`'s module
//!   documentation states it, and this file's
//!   `a_cache_entry_can_never_escape_the_downloads_directory` covers the
//!   half that *is* observable.
//! - **Corruption is never silently trusted** (Step 4) — the property
//!   this file exists for. A cached entry whose bytes stop matching its
//!   digest is evicted and reported as a miss, never handed back.

use std::path::{Path, PathBuf};

use admissionlab_core::cache::{CacheError, CacheLookup, CacheMiss, CachePaths, ContentKey};
use admissionlab_core::{RunId, sha256_hex};

// ---------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------

fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build test tokio runtime")
}

/// A fresh, guaranteed-unique cache root under the OS temp dir. Mirrors
/// the unique-temp-dir pattern the other test files in this crate use
/// rather than pulling in a new dependency for it.
fn unique_cache_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "admissionlab-core-cache-{label}-{}",
        RunId::generate().as_str()
    ))
}

/// The content every download test caches, plus a key naming it.
fn sample() -> (&'static [u8], ContentKey) {
    let bytes: &[u8] = b"a pinned, immutable release artifact";
    let key = ContentKey::for_content("release-v1.2.3.tar", bytes).expect("valid key");
    (bytes, key)
}

// ---------------------------------------------------------------------
// Where the cache root is
// ---------------------------------------------------------------------

/// An absolute override is used verbatim, and the three directories are
/// derived from it — never from the process's working directory or the
/// platform default.
#[test]
fn an_absolute_override_becomes_the_cache_root() {
    let paths = CachePaths::resolve_with(Some(Path::new("/var/tmp/admissionlab-cache")))
        .expect("an absolute override is accepted");

    assert_eq!(paths.root, PathBuf::from("/var/tmp/admissionlab-cache"));
    assert_eq!(paths.downloads, paths.root.join("downloads"));
    assert_eq!(paths.helm, paths.root.join("helm"));
    assert_ne!(
        paths.downloads, paths.helm,
        "the two caches must not share a directory"
    );
}

/// A relative override is rejected rather than resolved against the
/// working directory: the same configuration must never name different
/// directories depending on where Admission Lab was invoked from.
#[test]
fn a_relative_override_is_rejected() {
    let error = CachePaths::resolve_with(Some(Path::new("cache")))
        .expect_err("a relative override must be rejected");
    assert!(
        matches!(error, CacheError::NonAbsoluteCacheDir(_)),
        "expected NonAbsoluteCacheDir, got {error:?}"
    );
}

/// `ensure` creates all three directories and is safe to call twice.
#[test]
fn ensure_creates_every_directory_and_is_idempotent() {
    let root = unique_cache_root("ensure");
    let paths = CachePaths::rooted_at(&root);

    test_runtime().block_on(async {
        paths.ensure().await.expect("first ensure must succeed");
        paths.ensure().await.expect("second ensure must succeed");
    });

    assert!(paths.root.is_dir());
    assert!(paths.downloads.is_dir());
    assert!(paths.helm.is_dir());
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// What may be cached
// ---------------------------------------------------------------------

/// The containment guarantee: nothing [`ContentKey::new`] accepts can
/// produce a path outside the downloads directory, and every shape that
/// could is rejected at construction.
#[test]
fn a_cache_entry_can_never_escape_the_downloads_directory() {
    let digest = sha256_hex(b"content");
    let paths = CachePaths::rooted_at(Path::new("/cache/root"));

    for name in ["..", ".", "", "../escape", "sub/dir", "a\\b", "with space"] {
        let Err(error) = ContentKey::new(name, &digest) else {
            panic!("an unsafe entry name must be rejected: {name:?}");
        };
        assert!(
            matches!(error, CacheError::InvalidName { .. }),
            "expected InvalidName for {name:?}, got {error:?}"
        );
    }

    // And the digest half, which is the other component of the path.
    for bad_digest in [
        "",
        "abc",
        // Uppercase is rejected rather than normalized, so one digest
        // can never become two entries.
        &digest.to_uppercase(),
        // Sixty-four characters, but not all hexadecimal.
        &"z".repeat(64),
        // Sixty-five hexadecimal characters.
        &format!("{digest}a"),
    ] {
        let error = ContentKey::new("artifact.tar", bad_digest)
            .expect_err("a malformed digest must be rejected");
        assert!(
            matches!(error, CacheError::InvalidDigest { .. }),
            "expected InvalidDigest for {bad_digest:?}, got {error:?}"
        );
    }

    // Everything that *is* accepted lands under `downloads`.
    for name in ["kindest-node-v1.36.4.tar", "chart-1.2.3.tgz", "_a-B.9"] {
        let key = ContentKey::new(name, &digest).expect("a safe name is accepted");
        let path = paths.download_path(&key);
        assert!(
            path.starts_with(&paths.downloads),
            "{} escaped {}",
            path.display(),
            paths.downloads.display()
        );
        assert_eq!(path, paths.downloads.join(&digest).join(name));
    }
}

/// Storing and reading back the same content: a hit returns the real
/// path, and the bytes there are the bytes that went in.
#[test]
fn stored_content_is_returned_as_a_verified_hit() {
    let root = unique_cache_root("hit");
    let paths = CachePaths::rooted_at(&root);
    let (bytes, key) = sample();

    let (stored, lookup) = test_runtime().block_on(async {
        paths.ensure().await.expect("ensure must succeed");
        let stored = paths.store(&key, bytes).await.expect("store must succeed");
        let lookup = paths
            .verify_or_evict(&key)
            .await
            .expect("lookup must succeed");
        (stored, lookup)
    });

    assert_eq!(lookup, CacheLookup::Hit(stored.clone()));
    assert_eq!(
        std::fs::read(&stored).expect("the cached file must be readable"),
        bytes
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A key that was never stored is an ordinary, distinguishable miss —
/// not an error, and not an eviction.
#[test]
fn an_uncached_key_is_an_absent_miss() {
    let root = unique_cache_root("absent");
    let paths = CachePaths::rooted_at(&root);
    let (_, key) = sample();

    let lookup = test_runtime().block_on(async {
        paths.ensure().await.expect("ensure must succeed");
        paths
            .verify_or_evict(&key)
            .await
            .expect("a cold lookup must not error")
    });

    assert_eq!(lookup, CacheLookup::Miss(CacheMiss::Absent));
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------
// Corruption is never silently trusted (Task 5.6 Step 4)
// ---------------------------------------------------------------------

/// The headline property: a cached entry whose bytes no longer match the
/// digest naming it is **evicted and reported as a miss**, never returned.
///
/// Mutation check: an implementation that only checked for the file's
/// existence — the obvious way to write a cache — passes every other
/// test in this file and fails this one.
#[test]
fn a_corrupt_entry_is_evicted_and_never_returned() {
    let root = unique_cache_root("corrupt");
    let paths = CachePaths::rooted_at(&root);
    let (bytes, key) = sample();
    let tampered: &[u8] = b"something else entirely";

    let (stored, lookup, refilled) = test_runtime().block_on(async {
        paths.ensure().await.expect("ensure must succeed");
        let stored = paths.store(&key, bytes).await.expect("store must succeed");

        // Corrupt the entry behind the cache's back — a truncated
        // download, a bad disk, an edited file. The key is unchanged, so
        // only verification can catch this.
        std::fs::write(&stored, tampered).expect("tampering must succeed");

        let lookup = paths
            .verify_or_evict(&key)
            .await
            .expect("a corrupt lookup must evict, not error");

        // And the cache is usable again immediately: re-storing the real
        // content after an eviction works, which is what makes
        // "redownload" a real recovery rather than a permanent miss.
        paths
            .store(&key, bytes)
            .await
            .expect("restore must succeed");
        let refilled = paths
            .verify_or_evict(&key)
            .await
            .expect("the restored entry must verify");

        (stored, lookup, refilled)
    });

    assert_eq!(
        lookup,
        CacheLookup::Miss(CacheMiss::Evicted {
            actual_sha256: sha256_hex(tampered),
        }),
        "a mismatched entry must be reported as an eviction naming what was actually found"
    );
    assert_eq!(
        refilled,
        CacheLookup::Hit(stored.clone()),
        "the cache must be usable again after an eviction"
    );
    assert_eq!(
        std::fs::read(&stored).expect("the restored file must be readable"),
        bytes
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The eviction is a real deletion, not merely a reported miss: the
/// stale bytes are gone from disk, so nothing else can pick them up.
#[test]
fn eviction_removes_the_stale_bytes_from_disk() {
    let root = unique_cache_root("evict-file");
    let paths = CachePaths::rooted_at(&root);
    let (bytes, key) = sample();

    let stored = test_runtime().block_on(async {
        paths.ensure().await.expect("ensure must succeed");
        let stored = paths.store(&key, bytes).await.expect("store must succeed");
        std::fs::write(&stored, b"corrupt").expect("tampering must succeed");
        paths
            .verify_or_evict(&key)
            .await
            .expect("eviction must succeed");
        stored
    });

    assert!(
        !stored.exists(),
        "{} must have been deleted, not merely reported as a miss",
        stored.display()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The cache cannot be poisoned from the write side either: bytes that
/// do not hash to the key naming them are refused, and nothing is
/// written.
#[test]
fn storing_content_under_the_wrong_key_is_refused() {
    let root = unique_cache_root("poison");
    let paths = CachePaths::rooted_at(&root);
    let (_, key) = sample();
    let wrong: &[u8] = b"not what this key promises";

    let error = test_runtime().block_on(async {
        paths.ensure().await.expect("ensure must succeed");
        paths
            .store(&key, wrong)
            .await
            .expect_err("mismatched content must be refused")
    });

    match error {
        CacheError::DigestMismatch { expected, actual } => {
            assert_eq!(expected, key.sha256());
            assert_eq!(actual, sha256_hex(wrong));
        }
        other => panic!("expected DigestMismatch, got {other:?}"),
    }
    assert!(
        !paths.download_path(&key).exists(),
        "a refused store must write nothing"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `ContentKey::for_content` and `ContentKey::new` agree: hashing the
/// content and being told the digest produce the same key, and therefore
/// the same cache path.
#[test]
fn a_key_derived_from_content_matches_one_built_from_its_digest() {
    let bytes: &[u8] = b"identical either way";
    let derived = ContentKey::for_content("thing.tar", bytes).expect("valid key");
    let declared = ContentKey::new("thing.tar", &sha256_hex(bytes)).expect("valid key");

    assert_eq!(derived, declared);
    assert_eq!(derived.name(), "thing.tar");
    assert_eq!(derived.sha256(), sha256_hex(bytes));
}
