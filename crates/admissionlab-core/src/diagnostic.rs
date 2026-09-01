//! Secret-safe structured diagnostics.
//!
//! Admission Lab installs third-party Helm charts and admission webhooks
//! as untrusted workloads and handles Kubernetes Secrets while doing so.
//! [`Diagnostic`] is the one vocabulary every later crate (cluster
//! diagnostics, install failures, fixture capture, the final report)
//! reports through, so it is built around a single, load-bearing
//! guarantee: **a diagnostic can never be made to carry a secret value
//! into a log line or a serialized report.**
//!
//! That guarantee comes entirely from [`RedactedValue`]'s shape, not from
//! any masking or scrubbing performed at serialization time. Its
//! `Sensitive` variant stores no data at all — there is no field for a
//! secret to hide in — so no matter how `Diagnostic` is formatted
//! (`Serialize`, `Debug`, `Display`) or which future report format reads
//! it, a sensitive context entry has nothing to leak. Callers that would
//! otherwise be tempted to attach a secret "for debugging" have no way to
//! do so: the only two things a context value can be are a public string
//! or the fact that a value was withheld.
//!
//! # The other secret-safety primitive
//!
//! [`SensitiveBytes`] (ROADMAP Task 8.6, and §1.2's registry name) is
//! this module's second export, and it applies the same discipline one
//! step earlier. [`RedactedValue`] answers "may this value be
//! *rendered*?" by making a withheld value carry no payload at all;
//! [`SensitiveBytes`] is for the case where the payload genuinely has to
//! be carried — a generated TLS private key has to reach the one place
//! that writes it — and makes every *rendering* of it (`Debug`,
//! `Display`, `Serialize`) produce the identical [`REDACTED`] literal,
//! with the real bytes reachable only through one explicit, greppable
//! accessor.
//!
//! Both live here rather than in whichever crate happens to produce a
//! secret, because "how Admission Lab handles material it must not
//! print" is one vocabulary and Global Constraint 14 is one rule. A
//! second, crate-local version of either would be free to drift into
//! disagreeing with this one about what `[REDACTED]` even means.

use std::collections::BTreeMap;
use std::fmt;

// ROADMAP Task 7.2 (frozen `admissionlab.io/result/v1beta1` result
// schema): a `Diagnostic` reaches that document both at run level and
// inside every captured `AdmissionOutcome`, so the generated schema has
// to describe it. Derive only -- no field, name, or semantic change.
use schemars::JsonSchema;
use serde::{Serialize, Serializer};

/// The exact text every [`RedactedValue::Sensitive`] renders as, in every
/// format: `Debug`, `Display`, and JSON serialization all agree on this
/// one literal, so nothing about how a diagnostic is logged, printed, or
/// serialized can distinguish "redacted" from "some specific secret".
const REDACTED: &str = "[REDACTED]";

/// Severity of a [`Diagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticLevel {
    /// Informational; does not by itself indicate a problem.
    Info,
    /// A potential problem that did not stop the run.
    Warning,
    /// A definite problem.
    Error,
}

/// A context value that has been explicitly screened for secrecy.
///
/// This is the type that makes [`Diagnostic`] secret-safe. `Sensitive`
/// carries **no payload by design**: the value it stands in for is never
/// stored here, so it cannot leak through `Serialize`, `Debug`,
/// `Display`, or any future report format that reads a `Diagnostic`.
/// `Public` is the deliberate opposite — a value the caller has decided
/// is safe to display verbatim, such as a namespace or a resource name.
///
/// There is no third option and no way to attach a payload to
/// `Sensitive`: a caller that holds a secret and wants to note that it
/// existed must use `Sensitive`, which discards the value; it can never
/// pass the value through "just this once".
#[derive(Clone, PartialEq, Eq)]
pub enum RedactedValue {
    /// A value known to be safe to display and log verbatim.
    Public(String),
    /// A value that must never be displayed or logged. Carries no data.
    Sensitive,
}

impl fmt::Debug for RedactedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public(value) => f.debug_tuple("Public").field(value).finish(),
            Self::Sensitive => f.write_str(REDACTED),
        }
    }
}

impl fmt::Display for RedactedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public(value) => f.write_str(value),
            Self::Sensitive => f.write_str(REDACTED),
        }
    }
}

// Hand-written rather than derived: serde's derive would serialize this
// enum as an externally tagged value (for example
// `{"Sensitive":null}` or `{"Public":"..."}`), which both exposes the
// Rust variant name and does not match the plain-string contract every
// consumer (this crate's tests, the eventual report format) relies on.
// Writing it by hand also keeps the property that makes `Sensitive` safe
// impossible to violate by accident: there is no `self` data to serialize
// for that arm, only the fixed `REDACTED` literal.
impl Serialize for RedactedValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Public(value) => serializer.serialize_str(value),
            Self::Sensitive => serializer.serialize_str(REDACTED),
        }
    }
}

/// A byte string that must never be rendered: every format prints
/// [`REDACTED`], and the bytes are reachable only through
/// [`SensitiveBytes::expose`].
///
/// §1.2's registry freezes the name and the shape
/// (`SensitiveBytes(Vec<u8>)`). ROADMAP Task 8.6 is the first producer —
/// `admissionlab_gateway::tls` generates a TLS private key that has to
/// be *written somewhere* and so cannot use [`RedactedValue::Sensitive`],
/// which deliberately keeps nothing.
///
/// # What this guarantees, and what it does not
///
/// **Guaranteed:** no formatting of this value reveals it. `Debug`,
/// `Display`, and `Serialize` are hand-written to emit the same
/// [`REDACTED`] literal that [`RedactedValue::Sensitive`] emits, so a
/// struct holding one can derive `Debug` and be logged, `dbg!`-ed, or
/// serialized into a report without a reviewer having to check each
/// call site. That is the property worth having: leaks of this kind
/// happen through a `{:?}` somebody added to an unrelated struct, not
/// through a deliberate print.
///
/// **Not guaranteed:** that the bytes are unreachable, or that they are
/// erased from memory. [`SensitiveBytes::expose`] exists precisely so a
/// caller *can* reach them, because a private key that could never be
/// written would be useless; and this type does not zeroize on drop, so
/// it makes no claim about memory residency. Both are stated rather than
/// implied — a security primitive that overstates its scope is worse
/// than one that is narrow and honest (Global Constraint 15's habit,
/// applied to this crate's own guarantees).
///
/// # Deliberately not derived
///
/// - **No `Deserialize`.** Its `Serialize` is lossy on purpose, so a
///   round trip could only ever produce the literal `[REDACTED]` bytes
///   and call them a key. There is no format this value is *read* from;
///   it is constructed from freshly generated material.
/// - **No `PartialEq`/`Ord`/`Hash`.** Comparing secrets is not something
///   this project needs to do, and offering it invites both a timing
///   oracle and the habit of using a secret as a map key. A caller with
///   a real reason compares `expose()` slices explicitly, where the
///   choice is visible.
#[derive(Clone)]
pub struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    /// Wraps `bytes` as sensitive material.
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    ///
    /// **This is the leak.** Every call is a place where secret material
    /// escapes the wrapper, so the name is deliberately blunt and
    /// deliberately greppable: `expose` in a diff is a reviewable event,
    /// where `as_bytes` or a `Deref` impl would not be. Call it at the
    /// one site that writes the material to its destination, and nowhere
    /// else — never to build a log line, an error message, or a report
    /// field.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// How many bytes are held. Safe to render: a length is not the
    /// material, and "the key is 1704 bytes" is a genuinely useful thing
    /// for a diagnostic to be able to say.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no bytes are held at all — which for generated key
    /// material means something went wrong upstream.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// Hand-written for the same reason `RedactedValue`'s are, and to the
// same literal: a derived `Debug` would print the byte vector, which is
// exactly the accident this type exists to prevent. The length is not
// included -- not because it is secret, but because `[REDACTED]` reading
// identically everywhere is what makes "a redacted value looks like
// this" a fact a reader can rely on rather than a family of formats.
impl fmt::Debug for SensitiveBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for SensitiveBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl Serialize for SensitiveBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(REDACTED)
    }
}

/// One structured diagnostic message.
///
/// `code`, `message`, and `context` are canonical field names shared with
/// every later crate that reports through `Diagnostic`; later tasks may
/// add fields but must not rename these. All fields are public and
/// `Diagnostic` has no constructor: any combination of `code`, `message`,
/// and `context` is valid, and callers build one directly with struct
/// literal syntax.
///
/// `context` is a [`BTreeMap`] rather than a `HashMap` so that its
/// serialized key order is deterministic — required for reproducible
/// reports and diffable snapshots — and its values are [`RedactedValue`]
/// rather than `String` so that every context entry has been explicitly
/// marked public or sensitive before it can be attached at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Diagnostic {
    /// A short, stable machine-readable identifier, for example
    /// `"install.failed"`.
    pub code: String,
    /// A human-readable description of what happened.
    pub message: String,
    /// Structured detail supporting `message`. A value that might expose
    /// secret material (tokens, credentials, webhook CA bundles, and
    /// similar) must be [`RedactedValue::Sensitive`], never
    /// [`RedactedValue::Public`].
    ///
    /// Described to `schemars` as a plain `string` map because that is
    /// exactly what [`RedactedValue`]'s hand-written [`Serialize`] emits
    /// -- `Public`'s value verbatim, or the fixed [`REDACTED`] literal.
    /// Deriving `JsonSchema` on the enum instead would describe the Rust
    /// variants, which never appear on the wire.
    #[schemars(with = "BTreeMap<String, String>")]
    pub context: BTreeMap<String, RedactedValue>,
}
