//! Everything one Gateway route contract produced on one side of a
//! comparison.
//!
//! [`GatewayCaseResult`] is §1.2's canonical bundle, and the unit Tasks
//! 6.9 and 6.11 both consume: 6.9's comparator pairs a baseline and a
//! candidate one and derives the semantic changes between them; 6.11
//! puts it in a run's report. Defined here, at the end of Task 6.8,
//! because 6.8 supplies the last of its three fields — until
//! [`crate::probe::HttpProbeResult`] existed there was nothing to
//! bundle.
//!
//! # What it is, and what it deliberately is not
//!
//! It is *observation*, not *verdict*. Nothing here says whether the
//! route behaved acceptably, whether a probe met its
//! [`crate::model::HttpProbeContract::expected_status`], or how bad a
//! difference would be. That grading belongs to `admissionlab-diff` and
//! `admissionlab-policy`, which is the whole reason this crate can be
//! vendor-neutral: the Gateway engine reports what an implementation
//! did, and exactly one place in the project decides what that means
//! (Global Constraint 6; the same line
//! `crate::reconcile::ReconciliationEvidence` draws by carrying
//! `converged: bool` and `diagnostics` but no severity).
//!
//! `probes` may legitimately be empty. A route that never reconciled has
//! no data plane to send a request through, and Task 6.11 Step 3 records
//! that as an explicit skip with a reason rather than as a probe that
//! passed or failed — so an empty `probes` alongside a
//! `reconciliation.converged == false` is a complete, honest case
//! result, not a partial one.
//!
//! `Serialize` but not `Deserialize`, like every other evidence type in
//! this crate: it is captured once from a live cluster and only ever
//! travels outward into a report.

use serde::Serialize;

use crate::probe::HttpProbeResult;
use crate::reconcile::ReconciliationEvidence;

/// One route contract's complete result on one side.
///
/// No `Default`: an evidence type with one can be fabricated by
/// accident, and a `GatewayCaseResult` that nothing observed would claim
/// a route reconciled with no conditions and answered no probes — which
/// is a statement about a cluster, not an absence of one (Global
/// Constraint 15). The same reasoning
/// [`ReconciliationEvidence`] records for itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCaseResult {
    /// The [`crate::model::RouteContract::id`] this result is about,
    /// copied verbatim. Unique within a suite (enforced by
    /// `admissionlab_spec::resolve_lab`), which is what lets Task 6.9
    /// pair a baseline case with its candidate by id rather than by
    /// position — two runs need not have produced their cases in the
    /// same order, and pairing by index would silently compare unrelated
    /// routes.
    pub contract_id: String,
    /// What the implementation's controllers published about the route,
    /// its `Gateway`, and its `GatewayClass`.
    pub reconciliation: ReconciliationEvidence,
    /// What each of the contract's probes returned, in the order the
    /// contract declares them — see this module's documentation for why
    /// this may be empty.
    pub probes: Vec<HttpProbeResult>,
}
