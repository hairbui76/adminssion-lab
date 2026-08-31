//! The reusable, immutable, content-addressed cache: where Admission Lab
//! keeps things that are worth downloading once and reusing across runs
//! (Task 5.6).
//!
//! # What may live here, and what may never
//!
//! Exactly one kind of thing belongs in this cache: **immutable content,
//! addressed by the hash of its own bytes.** A `kind` node image tarball,
//! a Helm chart archive, a pinned release binary — anything whose
//! identity *is* its content, so that two runs asking for the same key
//! are asking for byte-identical data.
//!
//! Nothing run-scoped may ever live here: not a kubeconfig, not an audit
//! log, not a raw captured object, not a rendered report. Those are what
//! [`crate::artifact::ArtifactStore`] holds, they are single-use, and
//! several of them are credential material that
//! `admissionlab-core`'s permission discipline restricts to the owner
//! inside a run directory. Reusing any of them across runs would be a
//! correctness bug (a second run reading the first run's evidence) *and*
//! a Global Constraint 4 violation (baseline and candidate must not share
//! mutable state).
//!
//! ## The API shape is what enforces it
//!
//! That rule is not left to reviewer vigilance. **No method on
//! [`CachePaths`] takes a [`crate::RunId`], a [`crate::RunPaths`], or a
//! [`crate::Side`]**, and there is no way to construct one that does: the
//! only thing that names an entry is a [`ContentKey`], which is a
//! validated `(name, sha256)` pair. A run-scoped artifact has no content
//! hash known ahead of time and no stable name across runs, so it simply
//! cannot be addressed here. Task 5.6 Step 3 asks for exactly this. The
//! rule is a *compile-time* property, so no runtime test can assert it;
//! what `tests/cache.rs` does assert is the observable half —
//! `a_cache_entry_can_never_escape_the_downloads_directory` — that every
//! key which validates addresses something inside the cache, and every
//! shape that would not is rejected at construction.
//!
//! # Corruption is never silently trusted (Task 5.6 Step 4)
//!
//! A cache entry's bytes are verified against the digest that names them
//! **every time** they are looked up, not only when they are written.
//! [`CachePaths::verify_or_evict`] hashes what is on disk and:
//!
//! - returns [`CacheLookup::Hit`] when the digest matches;
//! - **deletes the file** and returns [`CacheLookup::Miss`] with
//!   [`CacheMiss::Evicted`] when it does not, carrying the digest that
//!   was actually found;
//! - returns [`CacheMiss::Absent`] when there is nothing there.
//!
//! Stale bytes are therefore never handed back, and the two kinds of miss
//! stay distinguishable: "nothing was cached" and "something was cached
//! and it was wrong" have very different diagnostic weight, and
//! collapsing them into one `Option` would hide a corrupted or tampered
//! cache behind an ordinary cold start. A caller that treats both the
//! same way still behaves correctly (it redownloads); a caller that wants
//! to warn about corruption can.
//!
//! [`CachePaths::store`] verifies in the other direction: it refuses to
//! write bytes that do not hash to the key naming them, so a caller
//! cannot poison the cache by handing a truncated download to the right
//! key.
//!
//! # Where the cache root is
//!
//! `ADMISSIONLAB_CACHE_DIR` overrides everything when set to a non-empty,
//! absolute path. Otherwise the platform convention applies (see
//! [`default_cache_root`]): `$XDG_CACHE_HOME` or `~/.cache` on Linux and
//! other Unixes, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on
//! Windows, each with an `admissionlab` directory beneath it.
//!
//! Resolved by a small, documented match on [`std::env::consts::OS`]
//! rather than by adding a directory-discovery dependency: the whole
//! behavior is three environment-variable lookups and a join, and
//! [`platform_default`] is a pure function of them, so every platform's
//! answer is testable from any host — which a crate that reads the real
//! environment internally would not be.
//!
//! # What is *not* here yet: the Helm repository cache
//!
//! [`CachePaths::helm`] exists and is documented, but nothing writes to
//! it today. `admissionlab_installer::helm` isolates Helm's client state
//! per run *and per side* — `HELM_REPOSITORY_CONFIG` and
//! `HELM_REPOSITORY_CACHE` both point under the run's own `logs`
//! directory, fixed at `HelmInstaller::new` time — precisely so a real
//! `helm repo add` can never touch the operator's own
//! `~/.config/helm/repositories.yaml`.
//!
//! Only *half* of that is safe to move here, and the distinction is the
//! reason this field is documented rather than wired:
//! `HELM_REPOSITORY_CONFIG` (`repositories.yaml`) is **mutable, per-side
//! state** that two concurrent installs would race on, and it must stay
//! run-scoped. `HELM_REPOSITORY_CACHE` (downloaded repository indexes and
//! chart archives) is the immutable download material this cache is for.
//! Making that change means altering `HelmInstaller`'s constructor and
//! its `helm_isolation_env`, which live in `admissionlab-installer` — so
//! it is that crate's change to make, against this documented seam,
//! rather than something this module can reach into.
//!
//! **Provenance, never authentication.** Like every other SHA-256 in this
//! crate, the digests here detect accidental corruption and confirm that
//! two runs read the same bytes. They are unkeyed and are not a defense
//! against an attacker who can write to the cache directory.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::artifact::{ArtifactError, write_bytes_atomically};
use crate::run_manifest::sha256_hex;

/// The environment variable that overrides the platform default cache
/// root.
pub const CACHE_DIR_ENV: &str = "ADMISSIONLAB_CACHE_DIR";

/// The directory name Admission Lab claims inside whichever platform
/// cache directory applies.
const CACHE_DIR_NAME: &str = "admissionlab";

/// Failure modes of cache resolution and cache reads/writes.
#[derive(Debug, Error)]
pub enum CacheError {
    /// No cache root could be determined: `ADMISSIONLAB_CACHE_DIR` was
    /// unset or empty, and this platform's own convention could not be
    /// resolved either (no `HOME`, no `XDG_CACHE_HOME`, no
    /// `LOCALAPPDATA`).
    ///
    /// Reported rather than silently falling back to a temporary
    /// directory: a "cache" that quietly evaporates between runs is
    /// worse than a clearly absent one, because it looks like it is
    /// working.
    #[error(
        "no cache directory could be determined for this platform; set {CACHE_DIR_ENV} to an \
         absolute path"
    )]
    NoCacheDirectory,

    /// `ADMISSIONLAB_CACHE_DIR` was set to a relative path.
    ///
    /// Rejected for the same reason [`crate::RunOptions::run_root`] is:
    /// a relative root resolves against the process's current working
    /// directory, so the same configuration would name different
    /// directories depending on where the user happened to invoke
    /// Admission Lab from — which is exactly the non-reproducibility a
    /// cache exists to remove.
    #[error("{CACHE_DIR_ENV} must be an absolute path, got {}", .0.display())]
    NonAbsoluteCacheDir(PathBuf),

    /// A [`ContentKey`] name was empty, or contained a character that is
    /// not safe to use as a single filesystem path segment.
    #[error("cache entry name {name:?} is invalid: {reason}")]
    InvalidName {
        /// The rejected name.
        name: String,
        /// Which rule it failed.
        reason: &'static str,
    },

    /// A [`ContentKey`] digest was not 64 lowercase hexadecimal
    /// characters.
    #[error("cache entry digest {digest:?} is not a lowercase-hex SHA-256")]
    InvalidDigest {
        /// The rejected digest.
        digest: String,
    },

    /// [`CachePaths::store`] was given bytes that do not hash to the key
    /// naming them.
    #[error("refusing to cache content as {expected}: its bytes hash to {actual}")]
    DigestMismatch {
        /// The digest the key claimed.
        expected: String,
        /// The digest the bytes actually have.
        actual: String,
    },

    /// A filesystem operation failed.
    #[error("failed to {operation} `{}`: {source}", .path.display())]
    Io {
        /// A short description of what was being attempted.
        operation: &'static str,
        /// The path the operation was acting on.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Writing a cache entry through this crate's shared atomic-write
    /// primitive failed. See [`crate::artifact`]'s "This store is not a
    /// cache" section for why the primitive is shared.
    #[error("failed to write a cache entry: {0}")]
    Write(#[source] ArtifactError),
}

impl CacheError {
    /// Builds a [`CacheError::Io`] describing an I/O failure while
    /// performing `operation` on `path`.
    fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// The cache's directory layout.
///
/// Constructing this performs **no filesystem IO**; it only computes
/// paths, mirroring [`crate::RunPaths`]. [`CachePaths::ensure`] is what
/// creates the directories.
///
/// See this module's documentation for what may live under these
/// directories, and for why nothing here can name a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePaths {
    /// The cache's own root: `<platform cache dir>/admissionlab`, or
    /// whatever `ADMISSIONLAB_CACHE_DIR` names.
    pub root: PathBuf,
    /// Content-addressed downloads: `<root>/downloads`. Every file
    /// beneath this is named by a [`ContentKey`] and verified against its
    /// digest on every read.
    pub downloads: PathBuf,
    /// Helm's own reusable download material: `<root>/helm`. Reserved
    /// for `HELM_REPOSITORY_CACHE` and nothing else — see this module's
    /// "What is *not* here yet" section for why `repositories.yaml` must
    /// stay run-scoped, and why the wiring is a seam rather than a
    /// finished feature.
    pub helm: PathBuf,
}

impl CachePaths {
    /// The layout rooted at `root`. No filesystem IO.
    #[must_use]
    pub fn rooted_at(root: &Path) -> Self {
        Self {
            downloads: root.join("downloads"),
            helm: root.join("helm"),
            root: root.to_path_buf(),
        }
    }

    /// Resolves the cache root from the environment and returns its
    /// layout: `ADMISSIONLAB_CACHE_DIR` when set to a non-empty absolute
    /// path, otherwise [`default_cache_root`].
    ///
    /// An empty `ADMISSIONLAB_CACHE_DIR` is treated as unset, matching
    /// how a shell's `VAR=` reads to a person; a *relative* one is an
    /// error rather than silently resolved against the working directory
    /// (see [`CacheError::NonAbsoluteCacheDir`]).
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::NonAbsoluteCacheDir`] if the override is
    /// relative, or [`CacheError::NoCacheDirectory`] if no override was
    /// given and this platform's convention could not be resolved.
    pub fn resolve() -> Result<Self, CacheError> {
        let override_root = std::env::var_os(CACHE_DIR_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Self::resolve_with(override_root.as_deref())
    }

    /// The pure core of [`CachePaths::resolve`]: the layout for an
    /// explicit `override_root`, or the platform default when it is
    /// `None`.
    ///
    /// Public rather than private for two reasons. It is the seam a
    /// future `--cache-dir` flag plugs into — a flag and an environment
    /// variable should not each grow their own copy of the
    /// absoluteness rule. And it is the only way this behavior can be
    /// tested at all: setting an environment variable is `unsafe` in
    /// Rust 2024, and this workspace forbids `unsafe_code` outright, so
    /// a test can never exercise [`CachePaths::resolve`]'s env lookup
    /// directly. Taking the value as a parameter is the same discipline
    /// [`platform_default`] applies to the platform rules.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::NonAbsoluteCacheDir`] if `override_root` is
    /// relative, or [`CacheError::NoCacheDirectory`] if it is `None` and
    /// this platform's convention could not be resolved.
    pub fn resolve_with(override_root: Option<&Path>) -> Result<Self, CacheError> {
        match override_root {
            Some(root) if root.is_absolute() => Ok(Self::rooted_at(root)),
            Some(root) => Err(CacheError::NonAbsoluteCacheDir(root.to_path_buf())),
            None => Ok(Self::rooted_at(&default_cache_root()?)),
        }
    }

    /// Creates every directory this layout names.
    ///
    /// Idempotent, like [`crate::ArtifactStore::create_run`]. Unlike that
    /// method it sets no owner-only permissions: nothing here is
    /// sensitive by construction (see this module's documentation), and
    /// narrowing a directory a user may deliberately share between their
    /// own tools would be a surprise, not a safeguard.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] if any directory could not be created.
    pub async fn ensure(&self) -> Result<(), CacheError> {
        for directory in [&self.root, &self.downloads, &self.helm] {
            tokio::fs::create_dir_all(directory)
                .await
                .map_err(|source| CacheError::io("create cache directory", directory, source))?;
        }
        Ok(())
    }

    /// Where `key`'s content lives: `<downloads>/<sha256>/<name>`.
    ///
    /// The digest is a directory rather than a filename prefix so the
    /// entry keeps its real name (`kindest-node-v1.36.4.tar`), which
    /// matters for anything that inspects the cache by hand and for tools
    /// that care about a file's extension. Both components are validated
    /// by [`ContentKey::new`], so this path can never escape
    /// [`CachePaths::downloads`].
    ///
    /// Pure: no filesystem IO, and it does not imply the file exists.
    #[must_use]
    pub fn download_path(&self, key: &ContentKey) -> PathBuf {
        self.downloads.join(&key.sha256).join(&key.name)
    }

    /// Looks `key` up, verifying whatever is on disk against the digest
    /// that names it, and evicting it if the two disagree.
    ///
    /// This is the whole of Task 5.6 Step 4: stale or corrupt bytes are
    /// never returned to a caller, and the difference between a cold
    /// cache and an evicted one stays visible. See this module's
    /// "Corruption is never silently trusted" section.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] if the entry exists but could not be
    /// read, or if a mismatched entry could not be removed. A failure to
    /// *evict* is an error rather than a silent miss on purpose: leaving
    /// known-bad bytes on disk and reporting an ordinary miss would make
    /// every subsequent run pay the same verification cost and discover
    /// the same corruption.
    pub async fn verify_or_evict(&self, key: &ContentKey) -> Result<CacheLookup, CacheError> {
        let path = self.download_path(key);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CacheLookup::Miss(CacheMiss::Absent));
            }
            Err(source) => return Err(CacheError::io("read cache entry", &path, source)),
        };

        let actual_sha256 = sha256_hex(&bytes);
        if actual_sha256 == key.sha256 {
            return Ok(CacheLookup::Hit(path));
        }

        tracing::warn!(
            path = %path.display(),
            expected = %key.sha256,
            actual = %actual_sha256,
            "evicting a cache entry whose bytes do not match the digest naming it"
        );
        tokio::fs::remove_file(&path)
            .await
            .map_err(|source| CacheError::io("evict corrupt cache entry", &path, source))?;

        Ok(CacheLookup::Miss(CacheMiss::Evicted { actual_sha256 }))
    }

    /// Stores `bytes` under `key`, atomically, and returns where they
    /// landed.
    ///
    /// Verifies before writing: bytes that do not hash to `key`'s digest
    /// are refused, so a truncated or substituted download cannot be
    /// filed under a key that promises something else. The write itself
    /// goes through this crate's shared atomic-write primitive, so a
    /// concurrent [`CachePaths::verify_or_evict`] sees either no entry or
    /// the complete one — never a half-written file it would then evict.
    ///
    /// Idempotent by construction: writing the same key twice writes the
    /// same bytes to the same path.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::DigestMismatch`] if `bytes` do not hash to
    /// `key`'s digest, [`CacheError::Io`] if the entry's directory could
    /// not be created, and [`CacheError::Write`] if the atomic write
    /// failed.
    pub async fn store(&self, key: &ContentKey, bytes: &[u8]) -> Result<PathBuf, CacheError> {
        let actual = sha256_hex(bytes);
        if actual != key.sha256 {
            return Err(CacheError::DigestMismatch {
                expected: key.sha256.clone(),
                actual,
            });
        }

        let path = self.download_path(key);
        let parent = self.downloads.join(&key.sha256);
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|source| CacheError::io("create cache entry directory", &parent, source))?;

        write_bytes_atomically(&path, bytes, None)
            .await
            .map_err(CacheError::Write)?;
        Ok(path)
    }
}

/// What a cache lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup {
    /// The entry exists and its bytes hash to the expected digest. The
    /// path is safe to read.
    Hit(PathBuf),
    /// There is nothing usable cached; the caller must fetch the content
    /// (and may then [`CachePaths::store`] it).
    Miss(CacheMiss),
}

/// Why a lookup missed.
///
/// Two variants rather than one, so "nothing was cached" and "something
/// was cached and it was wrong" stay distinguishable — see this module's
/// "Corruption is never silently trusted" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheMiss {
    /// Nothing was cached under this key.
    Absent,
    /// Something was cached, its bytes did not match the digest naming
    /// them, and it has been deleted.
    Evicted {
        /// What the evicted bytes actually hashed to. Recorded so a
        /// caller can report the corruption concretely rather than as
        /// "something was wrong".
        actual_sha256: String,
    },
}

/// The only thing that can name a cache entry: a file name plus the
/// SHA-256 its content must have.
///
/// Both halves are validated at construction, which is what makes
/// [`CachePaths::download_path`] incapable of escaping the cache root —
/// and what makes it impossible to address a run-scoped artifact here at
/// all, since nothing run-scoped has a content hash known up front (see
/// this module's "The API shape is what enforces it" section).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentKey {
    /// The entry's file name, a single safe path segment.
    name: String,
    /// The lowercase-hex SHA-256 of the entry's content.
    sha256: String,
}

impl ContentKey {
    /// Builds a key for content named `name` whose bytes hash to
    /// `sha256`.
    ///
    /// `name` must be a non-empty single path segment made of ASCII
    /// alphanumerics, `-`, `_`, and `.`, and must not be `.` or `..`.
    /// That allow-list (rather than a list of forbidden patterns) is what
    /// keeps a constructed key safe to join onto the cache root, the same
    /// discipline [`crate::RunId`] applies to run identifiers.
    ///
    /// `sha256` must be exactly 64 lowercase hexadecimal characters —
    /// the form [`crate::sha256_hex`] produces. Uppercase is rejected
    /// rather than normalized, so two spellings of the same digest can
    /// never become two cache entries.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidName`] or
    /// [`CacheError::InvalidDigest`] describing which rule was broken.
    pub fn new(name: &str, sha256: &str) -> Result<Self, CacheError> {
        if name.is_empty() {
            return Err(CacheError::InvalidName {
                name: name.to_owned(),
                reason: "it is empty",
            });
        }
        if name == "." || name == ".." {
            return Err(CacheError::InvalidName {
                name: name.to_owned(),
                reason: "it names a directory rather than a file",
            });
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(CacheError::InvalidName {
                name: name.to_owned(),
                reason: "it contains a character outside [A-Za-z0-9._-]",
            });
        }
        if sha256.len() != 64
            || !sha256
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() && c <= 'f')
        {
            return Err(CacheError::InvalidDigest {
                digest: sha256.to_owned(),
            });
        }

        Ok(Self {
            name: name.to_owned(),
            sha256: sha256.to_owned(),
        })
    }

    /// Builds a key for `bytes`, hashing them to derive the digest.
    ///
    /// The convenient half of the API, for a caller that already holds
    /// the content (a just-completed download) rather than a digest it
    /// was told to expect ahead of time.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidName`] if `name` is not a safe path
    /// segment. The digest cannot be invalid, since this computes it.
    pub fn for_content(name: &str, bytes: &[u8]) -> Result<Self, CacheError> {
        Self::new(name, &sha256_hex(bytes))
    }

    /// The entry's file name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The entry's expected content digest, lowercase hex.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// The platform's conventional cache directory for Admission Lab,
/// ignoring `ADMISSIONLAB_CACHE_DIR`.
///
/// # Errors
///
/// Returns [`CacheError::NoCacheDirectory`] if the environment does not
/// supply what this platform's convention needs.
pub fn default_cache_root() -> Result<PathBuf, CacheError> {
    let read = |name: &str| std::env::var_os(name).map(PathBuf::from);
    platform_default(
        std::env::consts::OS,
        read("HOME").as_deref(),
        read("XDG_CACHE_HOME").as_deref(),
        read("LOCALAPPDATA").as_deref(),
    )
    .ok_or(CacheError::NoCacheDirectory)
}

/// The pure core of [`default_cache_root`]: which directory `os`'s
/// convention names, given the three environment values it can depend
/// on.
///
/// Taking the environment as parameters rather than reading it is what
/// makes every platform's rule testable from any host (`tests/cache.rs`
/// checks all four branches on whatever machine CI runs on). The rules:
///
/// - **macOS** uses `~/Library/Caches/admissionlab`, and deliberately
///   does *not* consult `XDG_CACHE_HOME`: the XDG base-directory spec is
///   a freedesktop.org convention, and a macOS user who has that variable
///   set for some other tool would be surprised to find Admission Lab's
///   cache follow it. This matches what the `dirs` crate does, which is
///   the behavior most Rust tools a user already has installed will
///   exhibit.
/// - **Windows** uses `%LOCALAPPDATA%\admissionlab` — local, not
///   roaming: a multi-gigabyte image cache has no business being synced
///   between machines.
/// - **Every other target** (Linux and the other Unixes) follows XDG:
///   `$XDG_CACHE_HOME/admissionlab` when that is set to an absolute path,
///   otherwise `$HOME/.cache/admissionlab`. A relative `XDG_CACHE_HOME`
///   is ignored rather than honored, exactly as the specification
///   requires.
///
/// An absolute-path requirement applies to `HOME` too, for the same
/// reason: a relative `HOME` would make the cache root depend on the
/// working directory. "Absolute" is [`Path::is_absolute`], which follows
/// the *host* platform's own rules — correct at runtime, because `os` is
/// only ever [`std::env::consts::OS`], the target this binary was
/// compiled for. It does mean the unit tests below must use paths that
/// are absolute on whichever host runs them, rather than paths shaped
/// like the platform each branch is named after.
fn platform_default(
    os: &str,
    home: Option<&Path>,
    xdg_cache_home: Option<&Path>,
    local_app_data: Option<&Path>,
) -> Option<PathBuf> {
    fn absolute(path: Option<&Path>) -> Option<&Path> {
        path.filter(|path| path.is_absolute())
    }

    let base = match os {
        "macos" => absolute(home)?.join("Library").join("Caches"),
        "windows" => absolute(local_app_data)?.to_path_buf(),
        _ => match absolute(xdg_cache_home) {
            Some(xdg) => xdg.to_path_buf(),
            None => absolute(home)?.join(".cache"),
        },
    };
    Some(base.join(CACHE_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform rules, on every platform, from any host — the reason
    /// [`platform_default`] takes its environment as parameters.
    #[test]
    fn platform_defaults_follow_each_conventions_rules() {
        // Host-absolute paths throughout, for the reason
        // `platform_default`'s documentation gives.
        let home = Path::new("/home/dev");
        let xdg = Path::new("/var/cache/dev");
        let local = Path::new("/local-app-data");

        assert_eq!(
            platform_default("linux", Some(home), None, None),
            Some(PathBuf::from("/home/dev/.cache/admissionlab"))
        );
        assert_eq!(
            platform_default("linux", Some(home), Some(xdg), None),
            Some(PathBuf::from("/var/cache/dev/admissionlab"))
        );
        // macOS ignores XDG_CACHE_HOME even when it is set.
        assert_eq!(
            platform_default("macos", Some(home), Some(xdg), None),
            Some(PathBuf::from("/home/dev/Library/Caches/admissionlab"))
        );
        assert_eq!(
            platform_default("windows", None, None, Some(local)),
            Some(PathBuf::from("/local-app-data/admissionlab"))
        );
    }

    /// A relative `XDG_CACHE_HOME` is ignored (per the XDG spec) rather
    /// than joined onto the working directory, and a missing `HOME`
    /// leaves nothing to fall back to.
    #[test]
    fn relative_or_missing_environment_values_are_not_used() {
        assert_eq!(
            platform_default(
                "linux",
                Some(Path::new("/home/dev")),
                Some(Path::new("cache")),
                None
            ),
            Some(PathBuf::from("/home/dev/.cache/admissionlab")),
            "a relative XDG_CACHE_HOME must be ignored, not joined"
        );
        assert_eq!(platform_default("linux", None, None, None), None);
        assert_eq!(
            platform_default("macos", Some(Path::new("dev")), None, None),
            None
        );
        assert_eq!(
            platform_default("windows", Some(Path::new("/home/dev")), None, None),
            None
        );
    }
}
