//! The one place a [`LabResult`] is stripped of secret material.
//!
//! Global Constraint 14 requires reports to redact Secret data,
//! authorization headers, private keys, configured sensitive paths, and
//! credential-like values. [`redact_result`] is where that happens, for
//! every renderer at once: the terminal summary, the JSON artifact, and
//! the HTML page all render a value that has already been through here,
//! so a new renderer cannot forget to redact and none of them can drift
//! from the others.
//!
//! # The five rules
//!
//! 1. **Kubernetes Secret objects.** Any JSON object anywhere in the
//!    result whose `kind` is `"Secret"` has every `data` and
//!    `stringData` *value* replaced with [`REDACTED`]. Key names are
//!    kept: which keys a Secret carries, and whether that set changed
//!    between the two sides, is exactly the kind of behavior difference
//!    this tool exists to report. This is applied by walking the whole
//!    tree of every embedded [`serde_json::Value`] -- an
//!    `AdmissionOutcome::final_object`, a `SemanticChange`'s `baseline`
//!    or `candidate` payload, the value inside a webhook's JSON Patch
//!    operation -- so a Secret nested inside a `List`'s `items`, or
//!    inside a patch that adds one, is caught the same way a top-level
//!    one is.
//! 2. **Sensitive headers.** In *every* string the result carries --
//!    diagnostic messages, rejection messages, API-server warnings,
//!    divergence explanations -- an `Authorization:`, `Cookie:`,
//!    `Set-Cookie:`, `Proxy-Authorization:`, `X-Auth-Token:`, or
//!    `X-Api-Key:` header has its value replaced through end of line.
//!    The same names are also matched as *object keys*: a captured
//!    header map is structure, not text, and the string walker alone
//!    would never see `{"authorization": "Bearer ..."}` as a header.
//! 3. **Private keys and configured pointers.** A PEM block whose label
//!    contains `PRIVATE KEY` is replaced, marker to marker, with
//!    [`REDACTED_PRIVATE_KEY`]. Separately, each RFC 6901 pointer in
//!    [`RedactionRules::json_pointers`] is resolved against every
//!    embedded JSON payload's own root and, where it hits, that location
//!    is replaced with [`REDACTED`].
//! 4. **Credential-like environment values.** An object shaped like a
//!    Kubernetes `EnvVar` -- a string `name` and a string `value` --
//!    whose `name` matches a credential pattern
//!    ([`DEFAULT_ENV_NAME_PATTERNS`], plus any caller additions) has its
//!    `value` replaced with [`REDACTED`]. The entry itself, its `name`,
//!    and the surrounding change all survive: see "What redaction never
//!    does" below.
//! 5. Nothing else. See "The gap this leaves", also below.
//!
//! # What redaction never does
//!
//! **It never removes structure.** Every rule replaces a *value* in
//! place; no rule drops a field, an array element, a change, or a
//! fixture. That is Global Constraint 15 applied to secrets: the
//! existence of a difference is itself the evidence this tool reports,
//! and a redaction pass that deleted a `SemanticChange` because its
//! payload held a password would be reporting that the two stacks agreed
//! when they did not. A reader of a redacted result can always still see
//! *that* an environment variable's literal changed, *which* variable it
//! was, and *where*; only the two literals are gone.
//!
//! **It never mutates its input.** [`redact_result`] takes `&LabResult`
//! and returns a new one. The caller keeps the unredacted value and
//! decides, explicitly, which one to hand to a renderer.
//!
//! **It is idempotent.** Redacting an already-redacted result returns an
//! equal value: every replacement is a fixed literal that no rule
//! matches again ([`REDACTED`] contains no header, no PEM marker, and no
//! second literal to strip), and pointer targets simply resolve to the
//! same already-replaced location. This matters because nothing in the
//! pipeline tracks whether a given `LabResult` has been through here.
//!
//! # The gap this leaves
//!
//! Rule 4 recognizes the `EnvVar` *shape*. A credential parked under an
//! arbitrary map key (`{"dbPassword": "hunter2"}` in an annotation, a
//! `ConfigMap`, or a CRD's spec) is not auto-detected, and that is a
//! deliberate limit rather than an oversight: the obvious generalization
//! -- redact any string whose *key* looks credential-like -- destroys
//! information that is not secret and that a reader needs. A
//! `secretKeyRef`'s `key` field names a key inside a Secret; a
//! `secretName` names the Secret. Both match a naive "contains `key` or
//! `secret`" test, neither holds a credential, and blanking them would
//! hide exactly the rewiring `admissionlab-diff` carries `valueFrom`
//! blocks verbatim in order to show (see that crate's `workload` module
//! documentation). [`RedactionRules::json_pointers`] is the seam for
//! those cases: a user who knows their CRD holds a credential at a
//! specific path says so, and the pass honors it.
//!
//! # Relationship to the diff crate's `env` sanitization
//!
//! `admissionlab-diff`'s `workload` module already replaces container
//! `env` literals with descriptors in the *report-ready payloads it
//! builds*, and its own documentation is explicit that this "is not a
//! substitute for Task 4.10's central redaction pass". That is the
//! contract honored here: this module assumes nothing about what that
//! one already did. Rule 4 runs over every payload regardless of origin,
//! so an `env` literal that reaches a report through a channel the diff
//! crate does not sanitize -- a raw `final_object`, a webhook's JSON
//! Patch, a payload built by a comparison that has no `env` awareness --
//! is caught here. Where both passes touch the same value the result is
//! unchanged, because the diff crate's descriptor carries no literal for
//! this one to find.
//!
//! # `Diagnostic` is already safe
//!
//! [`admissionlab_core::RedactedValue::Sensitive`] carries no payload at
//! all, so a diagnostic's sensitive context entries have nothing to
//! leak and are passed through untouched. Its `Public` entries and the
//! `message` still go through the string rules: "the caller decided this
//! is safe to display" and "this contains no `Authorization:` header"
//! are different claims. `Diagnostic::code` is left verbatim -- it is a
//! short, stable machine identifier from a fixed vocabulary
//! (`"install.failed"`), never user data.

use admissionlab_admission::{
    AdmissionDecision, AdmissionOutcome, AdmissionTrace, WebhookInvocation,
};
use admissionlab_core::{
    ComponentTiming, Diagnostic, InstallStage, RedactedValue, SideInstallTiming, StageTimings,
};
use admissionlab_diff::{DivergenceEvidence, SemanticChange};
use admissionlab_gateway::{
    GatewayCaseResult, GatewayClassEvidence, GatewayEvidence, HttpProbeResult, ObservedCondition,
    ReconciliationEvidence, RouteEvidence, RouteParentStatus,
};
use admissionlab_policy::{ClassifiedChange, PolicyResult, StaleExpectation};
use json_patch::{AddOperation, PatchOperation, ReplaceOperation, TestOperation};
use serde_json::{Map, Value};

use crate::model::{
    AdmissionComparison, ComponentReport, EnvironmentReport, EnvironmentSummary, FixtureComparison,
    GatewayCaseComparison, LabResult,
};

/// The text every redacted value is replaced with.
///
/// Deliberately the same literal
/// [`admissionlab_core::RedactedValue::Sensitive`] renders as, so a
/// reader of a report cannot tell which of the two mechanisms withheld a
/// given value -- and, more usefully, so "redacted" reads identically
/// everywhere.
pub const REDACTED: &str = "[REDACTED]";

/// The text a PEM private-key block is replaced with.
///
/// Distinct from [`REDACTED`] because the thing removed is a whole
/// multi-line block rather than one field's value, and a reader seeing
/// this knows a private key was present -- which is itself worth
/// reporting.
pub const REDACTED_PRIVATE_KEY: &str = "[REDACTED PRIVATE KEY]";

/// The credential-name substrings rule 4 matches an environment
/// variable's `name` against, case-insensitively.
///
/// These are substrings, not whole names: `DB_PASSWORD`,
/// `password_file`, and `PGPASSWORD` all match `"password"`. The list
/// deliberately over-approximates -- `"key"` matches `MONKEY_HOST`, and
/// `"auth"` matches `AUTHOR_NAME` -- because the two failure modes are
/// not symmetric. A false positive blanks one literal a reader could
/// have seen, and the change, the variable name, and its location all
/// remain; a false negative prints a production credential into an
/// artifact that gets attached to a CI run. Callers who need a name
/// matched that this list misses add it through
/// [`RedactionRules::with_env_name_pattern`]; there is deliberately no
/// way to *remove* one, because narrowing redaction is not a knob this
/// tool offers.
pub const DEFAULT_ENV_NAME_PATTERNS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "secret",
    "token",
    "key",
    "credential",
    "auth",
    "signature",
    "session",
    "private",
    "salt",
];

/// The header names rule 2 recognizes, in both text and object-key
/// position.
///
/// Lowercase, and matched case-insensitively. `"set-cookie"` is listed
/// separately from `"cookie"` rather than relying on the substring:
/// text matching requires a word boundary before the name (so the
/// `cookie` inside `Set-Cookie` is not a match on its own), and object
/// keys are compared for equality.
pub const SENSITIVE_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-auth-token",
    "x-api-key",
];

/// What a run's redaction pass should do beyond its built-in rules.
///
/// [`RedactionRules::default`] is the whole of Global Constraint 14's
/// non-configurable half: Secret data, headers, private keys, and
/// credential-like environment names are always redacted, with or
/// without a configured rule. This type carries only the two
/// user-supplied additions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionRules {
    /// RFC 6901 JSON pointers to blank out, in addition to the built-in
    /// rules -- Global Constraint 14's "configured sensitive paths".
    ///
    /// Each pointer is resolved against **each embedded JSON payload's
    /// own root**, not against the whole serialized `LabResult`. A
    /// payload is an `AdmissionOutcome::final_object`, a
    /// `SemanticChange`'s `baseline`/`candidate`, or the `value` inside
    /// a webhook JSON Patch operation. That is the level a user can
    /// actually write a pointer for: `/data/license` or
    /// `/spec/template/spec/containers/0/env/3/value` names something in
    /// *their* object, and stays correct no matter which channel of the
    /// report that object surfaces through, whereas a pointer into the
    /// report document would have to encode array indices this crate
    /// chooses.
    ///
    /// A pointer that does not resolve in a given payload is a no-op for
    /// that payload, not an error: the same rule is applied to every
    /// payload in the run and most of them will not have the field. The
    /// empty pointer `""` addresses a payload's root and replaces the
    /// whole payload.
    pub json_pointers: Vec<String>,
    /// Extra credential-name substrings for rule 4, added to
    /// [`DEFAULT_ENV_NAME_PATTERNS`]. Matched case-insensitively.
    pub env_name_patterns: Vec<String>,
}

impl RedactionRules {
    /// The built-in rules and nothing else.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an RFC 6901 pointer to blank out. See
    /// [`RedactionRules::json_pointers`] for what it is resolved
    /// against.
    #[must_use]
    pub fn with_json_pointer(mut self, pointer: impl Into<String>) -> Self {
        self.json_pointers.push(pointer.into());
        self
    }

    /// Adds a credential-name substring for rule 4.
    #[must_use]
    pub fn with_env_name_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.env_name_patterns.push(pattern.into());
        self
    }

    /// The full credential-name pattern list, lowercased once so the
    /// per-value match is a plain substring test.
    fn credential_patterns(&self) -> Vec<String> {
        DEFAULT_ENV_NAME_PATTERNS
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .chain(
                self.env_name_patterns
                    .iter()
                    .map(|pattern| pattern.to_lowercase()),
            )
            .collect()
    }
}

/// Returns a copy of `result` with every secret this pass knows how to
/// find replaced.
///
/// See this module's documentation for the rules, for the guarantees
/// (pure, idempotent, structure-preserving), and for the gap rule 4
/// deliberately leaves.
#[must_use]
pub fn redact_result(result: &LabResult, rules: &RedactionRules) -> LabResult {
    let context = Context {
        credential_patterns: rules.credential_patterns(),
        json_pointers: &rules.json_pointers,
    };
    LabResult {
        // Not user data: this crate's own constant, checked against the
        // golden file rather than rewritten here.
        schema_version: result.schema_version.clone(),
        // `RunId`'s grammar (lowercase alphanumerics and `-`) cannot
        // express a header, a PEM block, or anything else these rules
        // look for.
        run_id: result.run_id.clone(),
        summary: result.summary,
        environments: redact_environments(&result.environments),
        fixtures: result
            .fixtures
            .iter()
            .map(|fixture| redact_fixture(fixture, &context))
            .collect(),
        policy: redact_policy(&result.policy, &context),
        diagnostics: result.diagnostics.iter().map(redact_diagnostic).collect(),
        timings: result.timings.as_ref().map(redact_timings),
    }
}

/// The rule inputs threaded through the whole walk.
///
/// Bundled rather than passed as two arguments so adding a third rule
/// input later is one change here instead of one at every recursive call
/// site.
struct Context<'a> {
    /// Lowercased credential-name substrings for rule 4.
    credential_patterns: Vec<String>,
    /// The caller's RFC 6901 pointers for rule 3.
    json_pointers: &'a [String],
}

/// Redacts both sides' environment descriptions.
///
/// Component names and versions are vendor-supplied strings that reach a
/// report verbatim, so they go through the string rules like every other
/// string; nothing here is expected to match, and that is the point of
/// applying it uniformly rather than reasoning per field about which
/// strings a webhook author could have influenced.
fn redact_environments(environments: &EnvironmentSummary) -> EnvironmentSummary {
    EnvironmentSummary {
        baseline: redact_environment(&environments.baseline),
        candidate: redact_environment(&environments.candidate),
    }
}

/// Redacts one side's environment description. See
/// [`redact_environments`].
fn redact_environment(environment: &EnvironmentReport) -> EnvironmentReport {
    EnvironmentReport {
        kubernetes: redact_string(&environment.kubernetes),
        components: environment
            .components
            .iter()
            .map(|component| ComponentReport {
                name: redact_string(&component.name),
                version: redact_string(&component.version),
            })
            .collect(),
    }
}

/// Redacts one run's stage timings.
///
/// Every duration is carried verbatim: a number of milliseconds has no
/// representation in which any of these four rules could match. The one
/// string in the whole structure is a component's name, and it goes
/// through [`redact_string`] for exactly the reason
/// [`redact_environment`] already runs the identical name through it --
/// the two are the same value, and a rule that fired on one but not the
/// other would leave the redacted document contradicting itself.
///
/// Rebuilt field by field rather than cloned, so a future field on
/// `StageTimings` that *can* carry user data is a compile error here
/// rather than a silent pass-through.
fn redact_timings(timings: &StageTimings) -> StageTimings {
    StageTimings {
        cluster_creation: timings.cluster_creation.clone(),
        installation: timings.installation.as_ref().map(|install| InstallStage {
            wall: install.wall,
            baseline: install.baseline.as_ref().map(redact_side_install),
            candidate: install.candidate.as_ref().map(redact_side_install),
        }),
        fixture_capture: timings.fixture_capture.clone(),
        gateway_suite: timings.gateway_suite.clone(),
        comparison: timings.comparison,
        reporting: timings.reporting,
        cleanup: timings.cleanup.clone(),
        elapsed: timings.elapsed,
    }
}

/// [`redact_timings`] for one side's per-component breakdown.
fn redact_side_install(install: &SideInstallTiming) -> SideInstallTiming {
    SideInstallTiming {
        elapsed: install.elapsed,
        components: install.components.as_ref().map(|components| {
            components
                .iter()
                .map(|component| ComponentTiming {
                    name: redact_string(&component.name),
                    elapsed: component.elapsed,
                })
                .collect()
        }),
    }
}

/// Redacts one compared unit's whole comparison.
///
/// `fixture_id` is carried verbatim for the same reason as
/// [`LabResult::run_id`]: its grammar cannot hold a secret.
fn redact_fixture(fixture: &FixtureComparison, context: &Context<'_>) -> FixtureComparison {
    FixtureComparison {
        fixture_id: fixture.fixture_id.clone(),
        admission: fixture
            .admission
            .as_ref()
            .map(|admission| redact_admission(admission, context)),
        gateway: fixture.gateway.as_ref().map(redact_gateway),
        changes: fixture
            .changes
            .iter()
            .map(|change| redact_classified_change(change, context))
            .collect(),
    }
}

/// Redacts both sides' Gateway evidence for one route contract (ROADMAP
/// Task 6.11).
///
/// Every value below is rebuilt field by field rather than cloned, for
/// the reason [`redact_timings`] gives for doing the same: a field added
/// later to any of `admissionlab-gateway`'s evidence types becomes a
/// compile error here instead of a silently unredacted payload. That
/// matters more here than almost anywhere else in this pass, because two
/// of these fields carry text this project does not author -- a
/// controller's own `reason` string and a data plane's own response
/// headers.
fn redact_gateway(comparison: &GatewayCaseComparison) -> GatewayCaseComparison {
    GatewayCaseComparison {
        baseline: redact_gateway_case(&comparison.baseline),
        candidate: redact_gateway_case(&comparison.candidate),
    }
}

/// One side's Gateway case result.
///
/// `contract_id` is carried verbatim: it is the identifier the run
/// correlates the two sides by, and rewriting it would break the pairing
/// the document is built on -- the same reasoning `fixture_id` gets.
fn redact_gateway_case(case: &GatewayCaseResult) -> GatewayCaseResult {
    GatewayCaseResult {
        contract_id: case.contract_id.clone(),
        reconciliation: redact_reconciliation(&case.reconciliation),
        probes: case.probes.iter().map(redact_probe).collect(),
    }
}

/// One route's reconciliation evidence.
///
/// Object names, namespaces, generations and the `converged` flag are
/// Kubernetes identifiers and numbers, which none of these rules can
/// match. What genuinely needs the pass is every controller-authored
/// `reason` (rule 2 -- an implementation is free to put anything in one)
/// and every [`Diagnostic`] message, which is already handled by the
/// same [`redact_diagnostic`] the run-level list uses. There is no
/// [`Context`] parameter for the same reason [`redact_diagnostic`] has
/// none: no embedded [`Value`] payload lives here for rules 1, 3 or 4 to
/// walk -- a Gateway change's payloads travel as
/// [`SemanticChange`]s, which [`redact_semantic_change`] already covers.
fn redact_reconciliation(evidence: &ReconciliationEvidence) -> ReconciliationEvidence {
    ReconciliationEvidence {
        gateway_class: evidence
            .gateway_class
            .as_ref()
            .map(|class| GatewayClassEvidence {
                name: redact_string(&class.name),
                accepted: redact_condition(&class.accepted),
            }),
        gateway: GatewayEvidence {
            identity: evidence.gateway.identity.clone(),
            conditions: redact_conditions(&evidence.gateway.conditions),
            generation: evidence.gateway.generation,
            gateway_class_name: evidence
                .gateway
                .gateway_class_name
                .as_deref()
                .map(redact_string),
        },
        route: RouteEvidence {
            namespace: redact_string(&evidence.route.namespace),
            name: redact_string(&evidence.route.name),
            generation: evidence.route.generation,
            parents: evidence
                .route
                .parents
                .iter()
                .map(|parent| RouteParentStatus {
                    parent: parent.parent.clone(),
                    controller_name: parent.controller_name.as_deref().map(redact_string),
                    conditions: redact_conditions(&parent.conditions),
                })
                .collect(),
        },
        elapsed: evidence.elapsed,
        converged: evidence.converged,
        diagnostics: evidence.diagnostics.iter().map(redact_diagnostic).collect(),
    }
}

/// A condition map, keyed by condition type.
///
/// Keys are Gateway API's own condition types (`Accepted`,
/// `ResolvedRefs`, `Programmed`) and are carried verbatim; only the
/// values go through [`redact_condition`].
fn redact_conditions(
    conditions: &std::collections::BTreeMap<String, ObservedCondition>,
) -> std::collections::BTreeMap<String, ObservedCondition> {
    conditions
        .iter()
        .map(|(type_name, condition)| (type_name.clone(), redact_condition(condition)))
        .collect()
}

/// One observed condition. Its `reason` is controller-authored text, so
/// it goes through the string rules like every other such string in this
/// document.
fn redact_condition(condition: &ObservedCondition) -> ObservedCondition {
    ObservedCondition {
        type_name: condition.type_name.clone(),
        state: condition.state,
        reason: condition.reason.as_deref().map(redact_string),
        observed_generation: condition.observed_generation,
    }
}

/// One HTTP probe result.
///
/// `response_headers` is the one place in this whole document where a
/// *live HTTP response's* headers are carried verbatim as structure, and
/// `admissionlab_gateway::probe` deliberately filters nothing out of it
/// -- it is evidence, and the crate that captured it is not the one that
/// decides what a report may show. This is where that decision is made,
/// and it is made the same way it is made for a captured webhook's
/// headers: rule 2's object-key form replaces the *value* of any
/// sensitive header name (`Set-Cookie` being the realistic one for a
/// data-plane response) while keeping the name, so that a header
/// appearing on one side and not the other stays a visible difference.
fn redact_probe(probe: &HttpProbeResult) -> HttpProbeResult {
    HttpProbeResult {
        status: probe.status,
        backend: probe.backend.as_deref().map(redact_string),
        response_headers: probe
            .response_headers
            .iter()
            .map(|(name, value)| {
                let redacted = if is_sensitive_header_name(name) {
                    REDACTED.to_owned()
                } else {
                    redact_string(value)
                };
                (name.clone(), redacted)
            })
            .collect(),
        response_body_sha256: probe.response_body_sha256.clone(),
        elapsed: probe.elapsed,
        attempts: probe.attempts,
    }
}

/// Redacts both captured outcomes and the divergence attributed between
/// them.
fn redact_admission(
    comparison: &AdmissionComparison,
    context: &Context<'_>,
) -> AdmissionComparison {
    AdmissionComparison {
        baseline: redact_outcome(&comparison.baseline, context),
        candidate: redact_outcome(&comparison.candidate, context),
        first_divergence: comparison.first_divergence.as_ref().map(redact_divergence),
    }
}

/// Redacts one side's captured admission outcome.
///
/// `total_latency` and `side` hold no text. `decision`, `warnings`, and
/// the whole trace do, and `final_object` is the single largest
/// attacker-influenced payload in the document -- it is whatever the
/// candidate stack's webhooks made of the fixture, which is exactly
/// where an injected Secret or credential would show up.
fn redact_outcome(outcome: &AdmissionOutcome, context: &Context<'_>) -> AdmissionOutcome {
    AdmissionOutcome {
        fixture_id: outcome.fixture_id.clone(),
        side: outcome.side,
        decision: redact_decision(&outcome.decision),
        warnings: outcome
            .warnings
            .iter()
            .map(|warning| redact_string(warning))
            .collect(),
        total_latency: outcome.total_latency,
        final_object: outcome
            .final_object
            .as_ref()
            .map(|object| redact_payload(object, context)),
        trace: redact_trace(&outcome.trace, context),
        diagnostics: outcome.diagnostics.iter().map(redact_diagnostic).collect(),
    }
}

/// Redacts a decision's human-readable message.
///
/// A rejection message is written by whichever webhook denied the
/// request and is echoed verbatim; a webhook that quotes the offending
/// field's value into its own message is common, and that value can be a
/// credential. `code` is a status number.
fn redact_decision(decision: &AdmissionDecision) -> AdmissionDecision {
    match decision {
        AdmissionDecision::Accepted => AdmissionDecision::Accepted,
        AdmissionDecision::Rejected { code, message } => AdmissionDecision::Rejected {
            code: *code,
            message: redact_string(message),
        },
        AdmissionDecision::UnsupportedDryRun { message } => AdmissionDecision::UnsupportedDryRun {
            message: redact_string(message),
        },
    }
}

/// Redacts a webhook trace. `evidence` is an enum; the invocations carry
/// the text and the patches.
fn redact_trace(trace: &AdmissionTrace, context: &Context<'_>) -> AdmissionTrace {
    AdmissionTrace {
        evidence: trace.evidence,
        invocations: trace
            .invocations
            .iter()
            .map(|invocation| redact_invocation(invocation, context))
            .collect(),
    }
}

/// Redacts one webhook invocation, including the JSON Patch it returned.
///
/// The patch is where a mutating webhook's *injected* material lives: a
/// sidecar's environment, an added volume mounting a Secret, an
/// annotation carrying a token. Redacting `final_object` alone would
/// miss it whenever the final object was not captured.
fn redact_invocation(invocation: &WebhookInvocation, context: &Context<'_>) -> WebhookInvocation {
    WebhookInvocation {
        configuration: redact_string(&invocation.configuration),
        webhook: redact_string(&invocation.webhook),
        round: invocation.round,
        index: invocation.index,
        mutated: invocation.mutated,
        patch: invocation.patch.as_ref().map(|operations| {
            operations
                .iter()
                .map(|operation| redact_patch_operation(operation, context))
                .collect()
        }),
        latency: invocation.latency,
        outcome: invocation.outcome,
    }
}

/// Redacts the payload inside one RFC 6902 operation.
///
/// Matched and rebuilt variant by variant rather than round-tripped
/// through `serde_json::Value`: a future `json_patch` variant carrying a
/// value would then be a compile error here instead of silently passing
/// an unredacted payload through. `path`/`from` are structural locators
/// -- see [`redact_semantic_change`] for why locators are carried
/// verbatim -- and `remove`/`move`/`copy` carry no value at all.
fn redact_patch_operation(operation: &PatchOperation, context: &Context<'_>) -> PatchOperation {
    match operation {
        PatchOperation::Add(add) => PatchOperation::Add(AddOperation {
            path: add.path.clone(),
            value: redact_payload(&add.value, context),
        }),
        PatchOperation::Replace(replace) => PatchOperation::Replace(ReplaceOperation {
            path: replace.path.clone(),
            value: redact_payload(&replace.value, context),
        }),
        PatchOperation::Test(test) => PatchOperation::Test(TestOperation {
            path: test.path.clone(),
            value: redact_payload(&test.value, context),
        }),
        PatchOperation::Remove(_) | PatchOperation::Move(_) | PatchOperation::Copy(_) => {
            operation.clone()
        }
    }
}

/// Redacts a graded change without touching its grade.
///
/// `severity` and `expected` are `admissionlab-policy`'s decisions and
/// are carried through untouched: redaction removes secret *values*, and
/// changing what a run concluded would be a different thing entirely
/// (§1.1: report rendering never decides severity).
fn redact_classified_change(
    classified: &ClassifiedChange,
    context: &Context<'_>,
) -> ClassifiedChange {
    ClassifiedChange {
        change: redact_semantic_change(&classified.change, context),
        severity: classified.severity,
        expected: classified.expected,
    }
}

/// Redacts a semantic change's two payloads and its attribution.
///
/// `kind`, `fixture_id`, and `object_path` are carried verbatim.
/// `object_path` is an RFC 6901 pointer into the compared object -- a
/// structural locator naming *where* something changed, built by
/// `admissionlab-diff` from field names and array indices. It is not a
/// value, and rewriting it would break the one thing a reader uses it
/// for. `subject` (a container or webhook name) is vendor-influenced
/// text and does go through the string rules.
///
/// The two payloads are where the actual before/after values live, and
/// they get the full treatment. What survives is the point: after
/// redaction the change still says an `environment_changed` occurred, in
/// which container, at which path -- only the two literals are gone.
fn redact_semantic_change(change: &SemanticChange, context: &Context<'_>) -> SemanticChange {
    SemanticChange {
        kind: change.kind,
        fixture_id: change.fixture_id.clone(),
        object_path: change.object_path.clone(),
        subject: change.subject.as_deref().map(redact_string),
        baseline: change
            .baseline
            .as_ref()
            .map(|value| redact_payload(value, context)),
        candidate: change
            .candidate
            .as_ref()
            .map(|value| redact_payload(value, context)),
        origin: change.origin.as_ref().map(redact_divergence),
    }
}

/// Redacts a divergence attribution's free text and webhook names.
///
/// `confidence` and both positions are structural and are carried
/// through unchanged -- a renderer has to be able to label an
/// `inferred` or `unknown` attribution as such (Global Constraint 15),
/// and redaction must never be the reason it cannot.
fn redact_divergence(evidence: &DivergenceEvidence) -> DivergenceEvidence {
    DivergenceEvidence {
        confidence: evidence.confidence,
        baseline_position: evidence.baseline_position,
        candidate_position: evidence.candidate_position,
        baseline_webhook: evidence.baseline_webhook.as_deref().map(redact_string),
        candidate_webhook: evidence.candidate_webhook.as_deref().map(redact_string),
        explanation: redact_string(&evidence.explanation),
    }
}

/// Redacts the policy section.
///
/// `disposition` is the run's verdict and is untouched. A stale
/// expectation's `id` is a user-authored identifier from their own
/// `expectations.yaml`; its `reason` is generated prose that can quote
/// the run's own values, so it goes through the string rules.
fn redact_policy(policy: &PolicyResult, context: &Context<'_>) -> PolicyResult {
    PolicyResult {
        disposition: policy.disposition,
        changes: policy
            .changes
            .iter()
            .map(|change| redact_classified_change(change, context))
            .collect(),
        stale_expectations: policy
            .stale_expectations
            .iter()
            .map(|stale| StaleExpectation {
                id: stale.id.clone(),
                reason: redact_string(&stale.reason),
            })
            .collect(),
    }
}

/// Redacts a diagnostic. See this module's "`Diagnostic` is already
/// safe" section for why `Sensitive` context entries need nothing done
/// to them and why `code` is left alone.
fn redact_diagnostic(diagnostic: &Diagnostic) -> Diagnostic {
    Diagnostic {
        code: diagnostic.code.clone(),
        message: redact_string(&diagnostic.message),
        context: diagnostic
            .context
            .iter()
            .map(|(key, value)| {
                let redacted = match value {
                    RedactedValue::Public(text) => RedactedValue::Public(redact_string(text)),
                    RedactedValue::Sensitive => RedactedValue::Sensitive,
                };
                (key.clone(), redacted)
            })
            .collect(),
    }
}

/// Applies every value rule to one embedded JSON payload.
///
/// This is the entry point for rule 3's pointers, which resolve against
/// *this* value's root (see [`RedactionRules::json_pointers`]). The
/// recursive rules run first so that a pointer's replacement is the last
/// word for its location.
fn redact_payload(value: &Value, context: &Context<'_>) -> Value {
    let mut redacted = redact_value(value, context);
    for pointer in context.json_pointers {
        if let Some(target) = redacted.pointer_mut(pointer) {
            *target = Value::String(REDACTED.to_owned());
        }
    }
    redacted
}

/// Walks one JSON tree applying rules 1, 2 (object-key form), and 4, and
/// the string rules to every string it contains.
///
/// Rules are checked per entry, not per object, so an object can be a
/// Secret *and* hold a credential-named environment entry, and both
/// apply.
fn redact_value(value: &Value, context: &Context<'_>) -> Value {
    match value {
        Value::String(text) => Value::String(redact_string(text)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(item, context))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(redact_object(fields, context)),
        // Numbers, booleans, and null hold no text.
        other => other.clone(),
    }
}

/// The object arm of [`redact_value`].
fn redact_object(fields: &Map<String, Value>, context: &Context<'_>) -> Map<String, Value> {
    let is_secret = fields.get("kind").and_then(Value::as_str) == Some("Secret");
    // Rule 4's shape test: a Kubernetes `EnvVar` is `{name, value}` with
    // both strings. Requiring both means a `{name, valueFrom}` entry --
    // a *reference*, which holds no literal -- is never touched, and
    // neither is an unrelated object that happens to have a `name`.
    let is_credential_env = fields
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| matches_credential_pattern(name, &context.credential_patterns))
        && fields.get("value").is_some_and(Value::is_string);

    let mut redacted = Map::with_capacity(fields.len());
    for (key, entry) in fields {
        // Rule 4 (a credential-named `EnvVar`'s literal) and rule 2's
        // object-key form (a captured header map's value) blank the same
        // way; they are two separate reasons for one replacement, so
        // they share an arm rather than being written twice.
        let blank_entry = (is_credential_env && key == "value")
            || (entry.is_string() && is_sensitive_header_name(key));
        let value = if is_secret && (key == "data" || key == "stringData") {
            redact_secret_payload(entry)
        } else if blank_entry {
            Value::String(REDACTED.to_owned())
        } else {
            redact_value(entry, context)
        };
        redacted.insert(key.clone(), value);
    }
    redacted
}

/// Blanks every value in a Secret's `data`/`stringData` map, keeping the
/// keys.
///
/// A `data` block that is somehow not an object -- a malformed or
/// partially-applied patch payload -- is replaced wholesale rather than
/// recursed into: this pass cannot know which part of a shape it does
/// not recognize is the secret, and for a field named `data` on an
/// object declaring itself a Secret, all of it is the safe answer.
fn redact_secret_payload(value: &Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .keys()
                .map(|key| (key.clone(), Value::String(REDACTED.to_owned())))
                .collect(),
        ),
        _ => Value::String(REDACTED.to_owned()),
    }
}

/// Whether `name` contains any credential pattern, case-insensitively.
fn matches_credential_pattern(name: &str, patterns: &[String]) -> bool {
    let lowered = name.to_lowercase();
    patterns.iter().any(|pattern| lowered.contains(pattern))
}

/// Whether `key` is one of [`SENSITIVE_HEADER_NAMES`], compared
/// case-insensitively and for equality.
///
/// Equality rather than substring: a map key is a whole header name, and
/// a substring test here would blank an unrelated field named
/// `authorizationMode`.
fn is_sensitive_header_name(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    SENSITIVE_HEADER_NAMES
        .iter()
        .any(|header| *header == lowered)
}

/// Applies both string rules: header values, then private-key blocks.
///
/// Order does not matter (a PEM block contains no header line and a
/// header value contains no PEM marker), but it is fixed for
/// determinism.
fn redact_string(text: &str) -> String {
    redact_private_keys(&redact_headers(text))
}

/// Replaces the value of every recognized header, from after the colon
/// through end of line.
///
/// Matching is done on an ASCII-lowercased copy of `text`.
/// [`str::to_ascii_lowercase`] changes only ASCII bytes, so every byte
/// offset found in the copy addresses the same position in the original
/// -- which is what lets the original's bytes be copied out by index
/// without a second case-insensitive search.
///
/// The value ends at the first `\n` or `\r`, or at end of string.
/// Redacting to end of line rather than to end of string is what keeps a
/// multi-line dump of headers readable: only the sensitive values go.
fn redact_headers(text: &str) -> String {
    let lowered = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(value_start) = next_header_value_start(&lowered, cursor) {
        let value_end = text[value_start..]
            .find(['\n', '\r'])
            .map_or(text.len(), |offset| value_start + offset);
        out.push_str(&text[cursor..value_start]);
        out.push_str(REDACTED);
        cursor = value_end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Finds the start of the earliest recognized header's value at or after
/// `from` in an already-lowercased string.
///
/// A name matches only at a word boundary -- start of string, or
/// preceded by something that is not an ASCII alphanumeric, `-`, or `_`.
/// That is what stops `cookie` from matching inside `set-cookie` (which
/// is listed on its own) and `x-authorization-mode` from being read as
/// an `authorization` header.
fn next_header_value_start(lowered: &str, from: usize) -> Option<usize> {
    let bytes = lowered.as_bytes();
    let mut best: Option<usize> = None;

    for header in SENSITIVE_HEADER_NAMES {
        let mut search = from;
        while let Some(offset) = lowered[search..].find(header) {
            let start = search + offset;
            search = start + 1;

            if start > 0 {
                let previous = bytes[start - 1];
                if previous.is_ascii_alphanumeric() || previous == b'-' || previous == b'_' {
                    continue;
                }
            }

            let mut index = skip_spaces(bytes, start + header.len());
            if bytes.get(index) != Some(&b':') {
                continue;
            }
            index = skip_spaces(bytes, index + 1);

            best = Some(best.map_or(index, |current| current.min(index)));
            break;
        }
    }

    best
}

/// Advances `index` past ASCII spaces and tabs.
fn skip_spaces(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ' | b'\t')) {
        index += 1;
    }
    index
}

/// Replaces every PEM block whose label contains `PRIVATE KEY` with
/// [`REDACTED_PRIVATE_KEY`], from the opening `-----BEGIN` through the
/// closing `-----END ...-----`.
///
/// A block whose label is something else -- `CERTIFICATE`, `PUBLIC KEY`
/// -- is left alone: a certificate is public material a reader may need,
/// and blanking it would remove evidence for no benefit.
///
/// A truncated block (an opening marker with no matching close, which is
/// what a log line clipped mid-key looks like) is redacted through end
/// of string. Half a private key is still key material.
fn redact_private_keys(text: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    const MARKER: &str = "-----";

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(offset) = text[cursor..].find(BEGIN) {
        let start = cursor + offset;
        let label_start = start + BEGIN.len();
        let Some(label_offset) = text[label_start..].find(MARKER) else {
            // An opening marker with no terminating `-----` at all is
            // not a PEM header; leave the remainder as-is.
            break;
        };
        let after_label = label_start + label_offset + MARKER.len();

        if !text[label_start..label_start + label_offset]
            .to_ascii_uppercase()
            .contains("PRIVATE KEY")
        {
            out.push_str(&text[cursor..after_label]);
            cursor = after_label;
            continue;
        }

        out.push_str(&text[cursor..start]);
        out.push_str(REDACTED_PRIVATE_KEY);
        cursor = match text[after_label..].find(END) {
            Some(end_offset) => {
                let end_label_start = after_label + end_offset + END.len();
                text[end_label_start..]
                    .find(MARKER)
                    .map_or(text.len(), |tail| end_label_start + tail + MARKER.len())
            }
            None => text.len(),
        };
    }

    out.push_str(&text[cursor..]);
    out
}
