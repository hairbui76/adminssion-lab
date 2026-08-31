//! The capture port: `admissionlab_core::FixtureCapture`, plus the one
//! thing the comparison stage needs that the core trait cannot express.
//!
//! `admissionlab_core::LabRunner::capture_fixtures` drives a
//! [`FixtureCapture`] and hands back a `CapturedLab` — which, by
//! deliberate design, names *where each fixture's evidence is on disk*
//! and carries no admission outcome at all. `admissionlab-core` cannot
//! name `admissionlab_admission::AdmissionOutcome` without closing a
//! dependency cycle (see `admissionlab_core::run`'s own module
//! documentation), and the `outcome.json` those paths point at cannot be
//! read back either: [`AdmissionOutcome`] is `Serialize`-only on purpose,
//! because the `admissionlab_core::Diagnostic` values it carries render
//! secrets as a fixed `[REDACTED]` literal that has no faithful inverse.
//!
//! So the comparison stage takes the outcomes from the capture
//! implementation itself. [`OutcomeCapture`] is that requirement written
//! down: a [`FixtureCapture`] that can also hand back what it observed.
//! `admissionlab_admission::KubeFixtureCapture` already retains them
//! (see its own documentation), so implementing this for it is a
//! forwarding method; a test double implements both halves directly.
//!
//! Declaring the trait here rather than in `admissionlab-admission` keeps
//! the requirement where the requirement comes from — this is the only
//! crate that both drives `capture_fixtures` and compares outcomes — and
//! avoids putting a trait in the capture crate that only one consumer
//! would ever implement.

use admissionlab_admission::{AdmissionOutcome, KubeFixtureCapture};
use admissionlab_core::FixtureCapture;

/// A [`FixtureCapture`] that also reports the outcomes it observed.
///
/// The supertrait bound is what lets a caller pass an implementation
/// straight to `LabRunner::capture_fixtures` (a `&Self` coerces to
/// `&dyn FixtureCapture`) without a second object or an upcast.
///
/// # Contract
///
/// [`OutcomeCapture::captured_outcomes`] returns every outcome captured
/// **so far**, on either side, in capture order — including after a
/// failed `capture_side`. That partial case is load-bearing: Task 4.14
/// writes whatever evidence exists before it gives up, and the fixtures
/// that were captured before the failing one are real observations.
pub trait OutcomeCapture: FixtureCapture {
    /// Every outcome captured so far, both sides interleaved. Each one
    /// names its own `side` and `fixture_id`, so a caller pairs them by
    /// identity rather than by position.
    fn captured_outcomes(&self) -> Vec<AdmissionOutcome>;
}

impl OutcomeCapture for KubeFixtureCapture {
    fn captured_outcomes(&self) -> Vec<AdmissionOutcome> {
        KubeFixtureCapture::captured_outcomes(self)
    }
}
