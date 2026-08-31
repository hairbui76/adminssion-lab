//! Reading the request-scoped window of a kube-apiserver audit log that
//! belongs to one fixture request (Task 3.5).
//!
//! Global Constraint 18 configures each ephemeral cluster's audit policy
//! at `Request` level, and Global Constraint 17 replays fixtures serially
//! within a cluster. Together those two make a byte offset a sufficient
//! correlation primitive: take an offset immediately *before* issuing one
//! fixture request ([`AuditLogReader::checkpoint`]), issue exactly that
//! one request, then read forward from the offset
//! ([`AuditLogReader::events_since`]) until the API server has finished
//! writing the request's own `ResponseComplete` event. Task 3.10's
//! `capture_fixture` is the consumer this shape exists for; Task 3.6
//! parses mutating-webhook annotations out of the events this module
//! returns, and Task 3.7 folds them into an
//! [`crate::outcome::AdmissionOutcome`].
//!
//! # Four things this module refuses to guess
//!
//! An audit log is an append-only file being written *concurrently* by a
//! process this project does not control, so almost every unusual thing
//! this module can see has both an innocent and an alarming reading. Each
//! one is resolved explicitly rather than by whichever branch is easier:
//!
//! 1. **A trailing line with no terminating newline is unfinished, not
//!    corrupt.** kube-apiserver writes an audit event and its `\n`
//!    without any cross-process atomicity guarantee this module may rely
//!    on, so a read that lands mid-write sees a JSON prefix. Treating
//!    that as malformed would turn an ordinary race into a fixture
//!    failure, so [`AuditLogReader::events_since`] keeps the bytes
//!    buffered and re-polls until the line completes or `deadline`
//!    expires. It is never parsed, never reported, and never counted as
//!    evidence while incomplete.
//! 2. **A complete line that will not parse is a diagnostic, not a fatal
//!    error.** Audit logs carry every request the cluster serves, from
//!    components this project did not write; one unparsable line says
//!    nothing about whether *this fixture's* event is present. It is
//!    recorded as an [`admissionlab_core::Diagnostic`] and reading
//!    continues. See "Why diagnostics need their own channel" below.
//! 3. **A file shorter than the checkpoint is an error, not a reason to
//!    start over.** Rotation or truncation between `checkpoint` and
//!    `events_since` means the bytes the checkpoint referred to are
//!    gone. Silently rereading from `0` would hand Task 3.10 events from
//!    *before* its own request and let it correlate the wrong one, so
//!    this is [`AuditError::Truncated`] instead (Global Constraint 15:
//!    the honest answer is "this evidence is unavailable", never a
//!    plausible substitute).
//! 4. **Deadline expiry is a successful, partial answer.** Returning
//!    `Err` on a timeout would make "the API server had not flushed the
//!    event yet" indistinguishable from "the audit log could not be
//!    read at all". `events_since` returns `Ok` with whatever complete
//!    events it did read; Task 3.10 re-polls with a fresh deadline when
//!    its own event is not in the batch.
//!
//! # Why `ResponseComplete` ends the wait
//!
//! `ResponseComplete` is the last stage kube-apiserver emits for a
//! request, and it is the only stage that carries the admission
//! annotations Task 3.6 needs (`RequestReceived` is written before
//! admission runs at all). Because fixtures are serial (GC17), the first
//! `ResponseComplete` to appear after a checkpoint is overwhelmingly the
//! fixture's own -- but this module deliberately does **not** assert
//! that: it only stops *waiting*, and returns every complete event it
//! read, leaving the actual `auditID`/`objectRef` matching to Task 3.10,
//! which knows which request it issued. Each poll pass drains every
//! complete line available before checking, so a `ResponseComplete` never
//! causes later lines already on disk to be left behind for the next
//! call.
//!
//! # Why diagnostics need their own channel
//!
//! [`AuditLogReader::events_since`] returns `Result<Vec<AuditEvent>, _>`
//! -- a roadmap-frozen signature with no third slot for the
//! non-fatal diagnostics point 2 above produces. Widening the return type
//! to a tuple or a wrapper struct was rejected: it would push the
//! "unparsable line" case into every call site including the ones that
//! never care, for something that is an *aggregate* property of the
//! reader rather than of one call. Instead the trait carries a separate
//! [`AuditLogReader::drain_diagnostics`] with a default empty
//! implementation, so a test double or a future in-memory reader that
//! cannot produce diagnostics implements nothing extra, and
//! [`FileAuditLogReader`] overrides it.
//!
//! That override needs interior mutability, because `events_since` takes
//! `&self` (the trait is `Send + Sync` and shared across the concurrent
//! baseline/candidate replay Task 3.10 performs). [`FileAuditLogReader`]
//! uses a plain [`std::sync::Mutex`] and takes it only to push a batch of
//! already-built diagnostics or to drain them -- **never across an
//! `await`**, so no `.await` in this module is ever holding a lock and
//! the future stays `Send` without a async-aware mutex.
//!
//! # Why an unparsable line's own text is never reported
//!
//! Global Constraint 14 requires reports to redact secret material, and a
//! `Request`-level audit log (GC18) contains request bodies -- including
//! `Secret` objects other cluster components create while a run is in
//! flight. So the diagnostic for an unparsable line records *that* a line
//! at a given byte offset failed and how (a JSON syntax/data/EOF
//! classification plus the parser's line/column), with the line itself as
//! [`admissionlab_core::RedactedValue::Sensitive`], which stores no
//! payload at all. Deliberately absent is `serde_json::Error`'s own
//! `Display` text: for a type error it embeds the offending *value*
//! (`invalid type: string "..."`), which is exactly the audit-log content
//! that must not reach a report.

use std::collections::BTreeMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use admissionlab_core::{ClusterHandle, Diagnostic, RedactedValue};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// The `stage` value kube-apiserver writes on the final audit event for a
/// request -- the only stage that carries admission annotations, and the
/// one [`AuditLogReader::events_since`] stops waiting on. See this
/// module's documentation ("Why `ResponseComplete` ends the wait").
pub const STAGE_RESPONSE_COMPLETE: &str = "ResponseComplete";

/// The default interval between poll passes in
/// [`FileAuditLogReader::events_since`], used by
/// [`FileAuditLogReader::new`].
///
/// Small on purpose: a fixture's audit event usually lands within a few
/// milliseconds of its response, and Global Constraint 17 makes every
/// fixture pay this latency serially, so a coarse interval would show up
/// directly as run duration. It is not zero because a busy-loop `stat`
/// on the audit log competes with the API server writing it.
/// [`FileAuditLogReader::with_poll_interval`] exists so tests can shrink
/// it further without this constant becoming a test-tuned value.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A position in an append-only audit log, taken immediately before a
/// fixture request is issued and handed back to
/// [`AuditLogReader::events_since`] afterwards.
///
/// `byte_offset` is a plain file offset rather than a line number or a
/// timestamp on purpose: it is the only one of the three that is exact,
/// cheap to take (`stat`), and monotonic under concurrent appends. Line
/// numbers require reading the whole file to compute, and timestamps
/// cannot separate two events written within the same microsecond.
///
/// Carries no `Default`. A defaulted `byte_offset: 0` would silently mean
/// "read the entire audit log from the beginning", which is precisely the
/// wrong-request correlation [`AuditError::Truncated`] exists to prevent
/// -- a caller must always state which offset it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AuditCheckpoint {
    /// Byte offset from the start of the audit log file. Events at or
    /// after this offset are the ones
    /// [`AuditLogReader::events_since`] reports.
    #[serde(rename = "byteOffset")]
    pub byte_offset: u64,
}

/// The subset of an `audit.k8s.io/v1` `Event` that Admission Lab needs,
/// plus the event's raw `annotations` map.
///
/// Deliberately a *subset*. A full `Event` carries `requestObject` and
/// `responseObject` at `RequestResponse` level and a `user` block on
/// every event; none of that is needed to correlate a fixture request or
/// reconstruct its webhook chain, and all of it is exactly the secret
/// material Global Constraint 14 keeps out of reports. Unknown and extra
/// JSON fields are ignored rather than rejected (no
/// `#[serde(deny_unknown_fields)]`): the audit schema gains fields across
/// Kubernetes releases, and this project supports three minor versions at
/// a time (GC10), so a new upstream field must never turn every event on
/// a newer cluster into a diagnostic.
///
/// Each field's wire tag is pinned with an explicit `#[serde(rename)]`
/// rather than left to derive from the Rust identifier: these are
/// Kubernetes' own names, not this project's, and the same tags are what
/// Task 3.10 writes to `raw/<side>/<fixture-id>/audit.json`, so a Rust
/// rename must never silently change either contract.
///
/// # What is required, and what is honestly absent
///
/// `audit_id`, `stage`, `verb`, and `request_uri` have no `#[serde(default)]`:
/// all four are non-`omitempty` in the upstream Go type, and all four are
/// what Task 3.10 matches its own request against. An object missing any
/// of them is not a usable audit event, and failing to parse it (into a
/// diagnostic) is honest -- unlike inventing an empty `auditID`, which
/// would silently match nothing or, worse, match another such event.
///
/// Every remaining field is `Option` (or an empty map) when the log did
/// not carry it, and is never filled in with a plausible substitute
/// (Global Constraint 15). `#[serde(default)]` on those fields defaults
/// only to `None`/empty -- the honest-unknown value -- which is the
/// opposite of the collapse [`crate::trace::TraceEvidence`] forbids a
/// `Default` for, where the default would have been a *positive* claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// The API server's own unique identifier for the request. Stable
    /// across every stage of the same request, which is what makes it
    /// the correlation key Task 3.10 matches on.
    #[serde(rename = "auditID")]
    pub audit_id: String,
    /// Which stage of the request this event describes, verbatim (for
    /// example `"RequestReceived"` or [`STAGE_RESPONSE_COMPLETE`]).
    ///
    /// Kept as a `String` rather than a typed enum: upstream can add
    /// stages, so any enum would need an `Other(String)` bucket anyway,
    /// and nothing in this project ever needs more than an equality
    /// check against one literal.
    #[serde(rename = "stage")]
    pub stage: String,
    /// The request verb, lowercase, as the API server recorded it (for
    /// example `"create"`, `"patch"`, `"get"`).
    #[serde(rename = "verb")]
    pub verb: String,
    /// The full request URI including its query string -- which is where
    /// a server-side dry-run request's own `?dryRun=All` appears (Global
    /// Constraint 16), so this is how Task 3.10 tells its fixture
    /// request apart from an ordinary write to the same resource.
    #[serde(rename = "requestURI")]
    pub request_uri: String,
    /// The client's `User-Agent`, when the log carried one. `None` means
    /// the header was absent or empty, never an invented client
    /// identity.
    #[serde(rename = "userAgent", default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Which object the request targeted, when the log carried an
    /// `objectRef`. `None` for requests with no single target object.
    #[serde(rename = "objectRef", default, skip_serializing_if = "Option::is_none")]
    pub object_ref: Option<AuditObjectRef>,
    /// The response `Status` the API server produced, when the log
    /// carried one -- absent on `RequestReceived` events, which are
    /// written before any response exists.
    #[serde(
        rename = "responseStatus",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub response_status: Option<AuditResponseStatus>,
    /// When the API server received the request.
    ///
    /// A real parsed instant, not the raw RFC 3339 microsecond string:
    /// Task 3.7 compares these against
    /// [`crate::execute::RawAdmissionResponse`]'s own
    /// [`std::time::SystemTime`] request window, and re-parsing a string
    /// at every comparison site is how two call sites end up disagreeing
    /// about what a timestamp meant. `jiff::Timestamp` converts to
    /// [`std::time::SystemTime`] infallibly via `From`.
    ///
    /// `None` when the log did not carry the field. Upstream marks it
    /// required, so this is not expected -- but an event whose timestamp
    /// is missing is still perfectly usable correlation evidence
    /// (`audit_id` matching is exact), and dropping the whole event
    /// would lose more than it protects.
    #[serde(
        rename = "requestReceivedTimestamp",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub request_received_timestamp: Option<jiff::Timestamp>,
    /// When this particular stage completed. Same type and same
    /// absent-is-`None` reasoning as
    /// [`AuditEvent::request_received_timestamp`].
    #[serde(
        rename = "stageTimestamp",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub stage_timestamp: Option<jiff::Timestamp>,
    /// The event's `annotations` map, verbatim and uninterpreted.
    ///
    /// This is the raw half of this type, and it is raw on purpose: it
    /// is where kube-apiserver records
    /// `mutation.webhook.admission.k8s.io/round_<r>_index_<i>`,
    /// `patch.webhook.admission.k8s.io/...` (at `Request` level, per
    /// Global Constraint 18), and
    /// `validation.webhook.admission.k8s.io/...`. **Task 3.6 parses
    /// those; this module does not touch them**, so that exactly one
    /// place in the codebase encodes what those keys and their JSON
    /// values mean.
    ///
    /// A [`BTreeMap`] rather than a `HashMap` for the same reason
    /// [`admissionlab_core::Diagnostic::context`] is one: deterministic
    /// key order, so the `audit.json` artifact Task 3.10 writes is
    /// byte-reproducible and diffable.
    #[serde(
        rename = "annotations",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub annotations: BTreeMap<String, String>,
}

impl AuditEvent {
    /// Whether this is the final ([`STAGE_RESPONSE_COMPLETE`]) event for
    /// its request -- the stage that carries admission annotations and
    /// the one [`AuditLogReader::events_since`] stops waiting on.
    #[must_use]
    pub fn is_response_complete(&self) -> bool {
        self.stage == STAGE_RESPONSE_COMPLETE
    }
}

/// The subset of an audit event's `objectRef` this project reads.
///
/// Every field is `Option` because every field is `omitempty` upstream:
/// a cluster-scoped resource has no `namespace`, a core-group resource
/// has no `apiGroup` (the empty core group is *encoded* as an absent
/// field, so `None` here means "core group", not "unknown group"), and
/// most requests have no `subresource`. None of them is ever defaulted to
/// a plausible value such as `"default"` for a missing namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditObjectRef {
    /// The target's API group, absent for the core (`v1`) group.
    #[serde(rename = "apiGroup", default, skip_serializing_if = "Option::is_none")]
    pub api_group: Option<String>,
    /// The target's API version.
    #[serde(
        rename = "apiVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub api_version: Option<String>,
    /// The plural resource name, for example `"pods"`.
    #[serde(rename = "resource", default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The subresource, for example `"status"`, when the request
    /// targeted one.
    #[serde(
        rename = "subresource",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub subresource: Option<String>,
    /// The target's namespace, absent for cluster-scoped resources.
    #[serde(rename = "namespace", default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// The target object's name.
    #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The subset of an audit event's `responseStatus` (a Kubernetes
/// `metav1.Status`) this project reads.
///
/// `code` is `u16` to match
/// [`crate::outcome::AdmissionDecision::Rejected`]'s own `code` field, so
/// Task 3.7 can carry an observed rejection code across without a lossy
/// conversion in between. Every field is `Option` and `omitempty`
/// upstream; in particular a `None` `code` means the log carried no code,
/// never a fabricated `0` (see `execute::classify_rejection`'s own
/// documentation for the same rule on the response side).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditResponseStatus {
    /// The HTTP status code the API server returned.
    #[serde(rename = "code", default, skip_serializing_if = "Option::is_none")]
    pub code: Option<u16>,
    /// A human-readable description of the status.
    #[serde(rename = "message", default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// A machine-readable reason, for example `"Forbidden"`.
    #[serde(rename = "reason", default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `"Success"` or `"Failure"`, when present.
    #[serde(rename = "status", default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Reading an audit log failed outright -- no partial answer is
/// available.
///
/// Deliberately narrow. An unfinished trailing line, an unparsable line,
/// and an expired deadline are all *not* errors (see this module's
/// documentation); every variant here is a case where the file itself
/// could not be read, or where continuing would mean reporting evidence
/// from the wrong request. Follows the `path` + `#[source]` shape
/// `admissionlab_fixtures::FixtureError` and
/// `admissionlab_recipes::RecipeError` already use, so a caller can
/// always name the file it was looking at.
#[derive(Debug, Error)]
pub enum AuditError {
    /// The audit log's size could not be determined (`stat` failed):
    /// it does not exist yet, or is not readable by this process.
    #[error("failed to stat audit log {}: {source}", .path.display())]
    Metadata {
        /// The audit log that could not be inspected.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The audit log could not be opened for reading.
    #[error("failed to open audit log {}: {source}", .path.display())]
    Open {
        /// The audit log that could not be opened.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// Reading bytes from the audit log failed after it was opened.
    #[error(
        "failed to read audit log {} from byte offset {offset}: {source}",
        .path.display()
    )]
    Read {
        /// The audit log that could not be read.
        path: PathBuf,
        /// The byte offset the read started from.
        offset: u64,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The audit log is now shorter than the offset already consumed --
    /// it was rotated or truncated -- so the bytes the checkpoint
    /// referred to no longer exist.
    ///
    /// Reported rather than recovered from. Restarting the read at `0`
    /// would return events written *before* the caller's own request,
    /// which Task 3.10 could then correlate as if they were its own; an
    /// explicit "this evidence is gone" is the only honest answer
    /// (Global Constraint 15).
    #[error(
        "audit log {} shrank to {current_length} bytes, below the {read_offset} bytes already \
         consumed: it was rotated or truncated, so the checkpointed events no longer exist",
        .path.display()
    )]
    Truncated {
        /// The audit log that shrank.
        path: PathBuf,
        /// The offset the reader had consumed up to -- initially the
        /// checkpoint's own [`AuditCheckpoint::byte_offset`], and higher
        /// if the shrink happened during a later poll pass.
        read_offset: u64,
        /// The file's length when the shrink was noticed.
        current_length: u64,
    },
}

/// The contract Task 3.10's `capture_fixture` reads a fixture's own
/// audit-log window through.
///
/// `Send + Sync` for the same reason [`crate::execute::AdmissionExecutor`]
/// is (see its documentation, and `admissionlab_core::ClusterManager`'s
/// before it): Task 3.10 replays fixtures against the baseline and
/// candidate clusters concurrently, so each side's reader is held across
/// an `await` on a task that may move between runtime threads. Within a
/// side the requests stay serial (Global Constraint 17) -- that is what
/// makes offset-based correlation deterministic in the first place.
#[async_trait]
pub trait AuditLogReader: Send + Sync {
    /// Records where the audit log currently ends, to be passed to
    /// [`AuditLogReader::events_since`] after a fixture request has been
    /// issued.
    ///
    /// Synchronous (unlike [`AuditLogReader::events_since`]) because it
    /// is a single `stat` with no waiting of any kind, and because the
    /// caller takes it in the few microseconds immediately before
    /// issuing its request -- exactly where an `.await` point would let
    /// the runtime interleave something else and widen the window the
    /// checkpoint is supposed to pin down.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] if the audit log's current end could not
    /// be determined at all -- for [`FileAuditLogReader`], an
    /// [`AuditError::Metadata`].
    fn checkpoint(&self) -> Result<AuditCheckpoint, AuditError>;

    /// Reads every complete audit event written at or after
    /// `checkpoint`, waiting until one of them is a
    /// [`STAGE_RESPONSE_COMPLETE`] event or `deadline` passes.
    ///
    /// Returns whatever complete events were read, in file order. A
    /// `deadline` that expires first is **not** an error: the result is
    /// `Ok` with fewer (possibly zero) events, and a caller whose own
    /// event is not in the batch re-polls with a fresh deadline. See
    /// this module's documentation for why an unfinished trailing line
    /// is waited on rather than reported, and why an unparsable complete
    /// line becomes a [`AuditLogReader::drain_diagnostics`] entry rather
    /// than an error.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] only when no honest partial answer exists:
    /// the file could not be stat'd, opened, or read
    /// ([`AuditError::Metadata`], [`AuditError::Open`],
    /// [`AuditError::Read`]), or it was rotated/truncated below the
    /// offset already consumed ([`AuditError::Truncated`]).
    async fn events_since(
        &self,
        checkpoint: &AuditCheckpoint,
        deadline: Instant,
    ) -> Result<Vec<AuditEvent>, AuditError>;

    /// Takes the non-fatal diagnostics accumulated since the last call,
    /// leaving none behind.
    ///
    /// Defaults to empty so an implementation that cannot produce
    /// diagnostics (a test double, an in-memory reader) implements
    /// nothing extra. See this module's documentation ("Why diagnostics
    /// need their own channel") for why these do not travel back through
    /// [`AuditLogReader::events_since`]'s own return type.
    fn drain_diagnostics(&self) -> Vec<Diagnostic> {
        Vec::new()
    }
}

/// The production [`AuditLogReader`]: reads a kube-apiserver audit log
/// from a path on the local filesystem.
///
/// The path is a host path, not a path inside the ephemeral node: the
/// `kind` cluster's audit log is bind-mounted out, which is what
/// [`admissionlab_core::ClusterHandle::audit_log`] already points at and
/// what [`FileAuditLogReader::for_cluster`] uses. So this type never
/// shells out to `kubectl` or `docker` to read it -- it is an ordinary
/// file being appended to by another process.
#[derive(Debug)]
pub struct FileAuditLogReader {
    /// The audit log file to read.
    path: PathBuf,
    /// How long to wait between poll passes while a
    /// [`STAGE_RESPONSE_COMPLETE`] event (or a trailing line's
    /// terminating newline) has not arrived yet.
    poll_interval: Duration,
    /// Non-fatal parse diagnostics awaiting a
    /// [`AuditLogReader::drain_diagnostics`] call. A plain
    /// [`std::sync::Mutex`], never held across an `await` -- see this
    /// module's documentation.
    diagnostics: Mutex<Vec<Diagnostic>>,
}

impl FileAuditLogReader {
    /// Creates a reader for `path`, polling at
    /// [`DEFAULT_POLL_INTERVAL`].
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_poll_interval(path, DEFAULT_POLL_INTERVAL)
    }

    /// Creates a reader for `path` with an explicit `poll_interval`.
    ///
    /// Exists so tests can drive the wait loop in milliseconds rather
    /// than making [`DEFAULT_POLL_INTERVAL`] itself a test-tuned
    /// compromise.
    #[must_use]
    pub fn with_poll_interval(path: impl Into<PathBuf>, poll_interval: Duration) -> Self {
        Self {
            path: path.into(),
            poll_interval,
            diagnostics: Mutex::new(Vec::new()),
        }
    }

    /// Creates a reader for `cluster`'s own audit log.
    ///
    /// The one-line convenience Task 3.10 uses, so no caller has to
    /// re-derive the path [`admissionlab_core::ClusterHandle::audit_log`]
    /// already holds -- the same discipline
    /// `admissionlab_fixtures::resources::client_for` follows for a
    /// cluster's kubeconfig.
    #[must_use]
    pub fn for_cluster(cluster: &ClusterHandle) -> Self {
        Self::new(cluster.audit_log.clone())
    }

    /// The audit log this reader reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The audit log's current length in bytes.
    async fn file_length(&self) -> Result<u64, AuditError> {
        tokio::fs::metadata(&self.path)
            .await
            .map(|metadata| metadata.len())
            .map_err(|source| AuditError::Metadata {
                path: self.path.clone(),
                source,
            })
    }

    /// Every byte from `offset` to the file's current end.
    ///
    /// Opens the file afresh on each pass rather than holding a handle
    /// open across the whole wait: the log is being appended to by
    /// another process, and reopening is what makes a rotation visible
    /// as a shrinking file (a retained handle would keep reading the
    /// unlinked inode) -- the condition [`AuditError::Truncated`]
    /// reports.
    async fn read_from(&self, offset: u64) -> Result<Vec<u8>, AuditError> {
        let mut file =
            tokio::fs::File::open(&self.path)
                .await
                .map_err(|source| AuditError::Open {
                    path: self.path.clone(),
                    source,
                })?;
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|source| AuditError::Read {
                path: self.path.clone(),
                offset,
                source,
            })?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .await
            .map_err(|source| AuditError::Read {
                path: self.path.clone(),
                offset,
                source,
            })?;
        Ok(buffer)
    }

    /// Records `batch` for a later [`AuditLogReader::drain_diagnostics`].
    ///
    /// Takes a fully built batch rather than one diagnostic at a time so
    /// the lock is taken once per poll pass, and -- more importantly --
    /// so this is a synchronous function with no `.await` inside it,
    /// making "the lock is never held across an await" a property of the
    /// signature rather than of careful reading.
    fn record_diagnostics(&self, batch: Vec<Diagnostic>) {
        if batch.is_empty() {
            return;
        }
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        diagnostics.extend(batch);
    }
}

#[async_trait]
impl AuditLogReader for FileAuditLogReader {
    fn checkpoint(&self) -> Result<AuditCheckpoint, AuditError> {
        let byte_offset = std::fs::metadata(&self.path)
            .map(|metadata| metadata.len())
            .map_err(|source| AuditError::Metadata {
                path: self.path.clone(),
                source,
            })?;
        Ok(AuditCheckpoint { byte_offset })
    }

    async fn events_since(
        &self,
        checkpoint: &AuditCheckpoint,
        deadline: Instant,
    ) -> Result<Vec<AuditEvent>, AuditError> {
        // Absolute offset of the next byte to read.
        let mut read_offset = checkpoint.byte_offset;
        // Bytes read but not yet terminated by a newline: the unfinished
        // trailing line this module waits on rather than parsing.
        let mut pending: Vec<u8> = Vec::new();
        // Absolute offset of `pending[0]`, so a diagnostic can name the
        // exact byte position of the line it could not parse.
        let mut pending_offset = checkpoint.byte_offset;
        let mut events: Vec<AuditEvent> = Vec::new();

        loop {
            let length = self.file_length().await?;
            if length < read_offset {
                return Err(AuditError::Truncated {
                    path: self.path.clone(),
                    read_offset,
                    current_length: length,
                });
            }

            let mut saw_response_complete = false;
            if length > read_offset {
                let chunk = self.read_from(read_offset).await?;
                // `usize -> u64` is lossless on every target this
                // project builds for; `unwrap_or` keeps the conversion
                // total without an unreachable panic, matching
                // `trace::duration_millis_option`'s own saturation.
                read_offset =
                    read_offset.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                pending.extend_from_slice(&chunk);

                let mut diagnostics = Vec::new();
                let mut consumed = 0usize;
                while let Some(relative) =
                    pending[consumed..].iter().position(|byte| *byte == b'\n')
                {
                    let end = consumed + relative;
                    let line_offset =
                        pending_offset.saturating_add(u64::try_from(consumed).unwrap_or(u64::MAX));
                    match parse_line(&pending[consumed..end]) {
                        LineOutcome::Blank => {}
                        LineOutcome::Event(event) => {
                            saw_response_complete |= event.is_response_complete();
                            events.push(*event);
                        }
                        LineOutcome::Unparsable(error) => {
                            diagnostics.push(unparsable_line_diagnostic(
                                &self.path,
                                line_offset,
                                &error,
                            ));
                        }
                    }
                    consumed = end + 1;
                }
                pending.drain(..consumed);
                pending_offset =
                    pending_offset.saturating_add(u64::try_from(consumed).unwrap_or(u64::MAX));
                self.record_diagnostics(diagnostics);
            }

            // Drain-then-decide: every complete line available at this
            // pass has already been parsed above, so stopping here never
            // strands an event that was already on disk.
            if saw_response_complete {
                return Ok(events);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(events);
            }
            let remaining = deadline.saturating_duration_since(now);
            tokio::time::sleep(self.poll_interval.min(remaining)).await;
        }
    }

    fn drain_diagnostics(&self) -> Vec<Diagnostic> {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *diagnostics)
    }
}

/// What one complete (newline-terminated) audit-log line turned out to
/// be.
enum LineOutcome {
    /// The line held nothing but whitespace. Not an event and not a
    /// problem: an empty line carries no claim to lose, so it is neither
    /// reported nor counted.
    Blank,
    /// A parsed event.
    Event(Box<AuditEvent>),
    /// A complete line that is not a parsable `audit.k8s.io/v1` event.
    Unparsable(serde_json::Error),
}

/// Classifies one complete line's bytes.
///
/// A trailing `\r` is stripped so a log written with CRLF line endings
/// parses identically -- `serde_json` would otherwise reject the stray
/// byte and turn every line into a diagnostic.
fn parse_line(line: &[u8]) -> LineOutcome {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().all(u8::is_ascii_whitespace) {
        return LineOutcome::Blank;
    }
    match serde_json::from_slice::<AuditEvent>(line) {
        Ok(event) => LineOutcome::Event(Box::new(event)),
        Err(error) => LineOutcome::Unparsable(error),
    }
}

/// Builds the [`Diagnostic`] for one unparsable complete line.
///
/// Carries *where* and *how* it failed, never *what* it said: the `line`
/// context entry is [`RedactedValue::Sensitive`], which stores no
/// payload, and the message uses [`json_error_category`] plus the
/// parser's line/column rather than `serde_json::Error`'s own `Display`,
/// which embeds the offending value for a type mismatch. See this
/// module's documentation ("Why an unparsable line's own text is never
/// reported") and Global Constraint 14.
fn unparsable_line_diagnostic(
    path: &Path,
    line_offset: u64,
    error: &serde_json::Error,
) -> Diagnostic {
    let category = json_error_category(error);
    let mut context = BTreeMap::new();
    context.insert(
        "path".to_string(),
        RedactedValue::Public(path.display().to_string()),
    );
    context.insert(
        "byte_offset".to_string(),
        RedactedValue::Public(line_offset.to_string()),
    );
    context.insert(
        "json_error".to_string(),
        RedactedValue::Public(category.to_string()),
    );
    context.insert(
        "json_column".to_string(),
        RedactedValue::Public(error.column().to_string()),
    );
    // The line's own bytes: withheld, not merely omitted, so a reader of
    // the diagnostic can tell that content existed and was deliberately
    // not carried.
    context.insert("line".to_string(), RedactedValue::Sensitive);
    Diagnostic {
        code: "audit.line_unparsable".to_string(),
        message: format!(
            "audit log line at byte offset {line_offset} is not a parsable audit.k8s.io/v1 \
             event ({category} error at column {}); the line's contents are withheld",
            error.column()
        ),
        context,
    }
}

/// A stable, value-free name for what kind of JSON failure `error` was.
///
/// `serde_json::Error::classify` returns a non-exhaustive-in-spirit
/// `Category`; mapping it to fixed strings here keeps the diagnostic's
/// `json_error` context value a stable machine-readable token rather than
/// a `Debug` rendering that could change between `serde_json` releases.
fn json_error_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    }
}

#[cfg(test)]
mod tests {
    use super::{LineOutcome, parse_line, unparsable_line_diagnostic};
    use admissionlab_core::RedactedValue;
    use std::path::Path;

    /// The mutation this test exists to kill: treating a blank line as
    /// an unparsable one, which would emit a diagnostic for every empty
    /// line an audit log happens to contain.
    #[test]
    fn a_whitespace_only_line_is_blank_not_unparsable() {
        assert!(matches!(parse_line(b"   \t"), LineOutcome::Blank));
        assert!(matches!(parse_line(b""), LineOutcome::Blank));
    }

    /// The mutation this test exists to kill: dropping the `\r` strip,
    /// which would turn every line of a CRLF-written log into a
    /// diagnostic.
    #[test]
    fn a_carriage_return_terminated_line_still_parses() {
        let line = br#"{"auditID":"a","stage":"ResponseComplete","verb":"get","requestURI":"/"}"#;
        let mut with_crlf = line.to_vec();
        with_crlf.push(b'\r');
        match parse_line(&with_crlf) {
            LineOutcome::Event(event) => assert_eq!(event.audit_id, "a"),
            LineOutcome::Blank | LineOutcome::Unparsable(_) => {
                panic!("a CRLF-terminated audit line must still parse")
            }
        }
    }

    /// The mutation this test exists to kill: putting
    /// `serde_json::Error`'s own `Display` text into the diagnostic,
    /// which for a type mismatch embeds the offending audit-log *value*
    /// -- exactly the content Global Constraint 14 keeps out of reports.
    #[test]
    fn an_unparsable_line_diagnostic_never_carries_the_offending_value() {
        let secret = "super-secret-token-value";
        let line = format!(
            r#"{{"auditID":"a","stage":"s","verb":"v","requestURI":"/","annotations":"{secret}"}}"#
        );
        let LineOutcome::Unparsable(error) = parse_line(line.as_bytes()) else {
            panic!("a string where a map is expected must not parse as an audit event");
        };
        assert!(
            error.to_string().contains(secret),
            "precondition: serde_json's own message embeds the offending value, which is why \
             this diagnostic must not use it"
        );

        let diagnostic = unparsable_line_diagnostic(Path::new("/tmp/audit.log"), 42, &error);
        assert_eq!(diagnostic.code, "audit.line_unparsable");
        assert!(!diagnostic.message.contains(secret));
        assert_eq!(
            diagnostic.context.get("line"),
            Some(&RedactedValue::Sensitive)
        );
        for value in diagnostic.context.values() {
            assert!(!value.to_string().contains(secret));
        }
    }
}
