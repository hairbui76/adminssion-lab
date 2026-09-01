//! Semantic validation rules applied while resolving a [`crate::LabSpec`].
//!
//! Every function here is a small, independently testable predicate over
//! already-parsed data; [`crate::resolve_lab`] (in `resolve.rs`) is what
//! sequences them while building a [`crate::ResolvedLab`]. Kept separate
//! from `resolve.rs` so each rule reads as a single, named check rather
//! than being folded into the resolution walk.
//!
//! Every rejection uses [`crate::error::SpecError::validation`], which
//! prefixes the message with a dotted locator into the document (for
//! example `baseline.kubernetes: ...`) — the same convention
//! `serde_norway`'s own parse errors follow, so a validation failure and a
//! parse failure read consistently.

use std::path::{Path, PathBuf};

use crate::component::ResolvedComponent;
use crate::error::SpecError;
use crate::model::InstallMethodSpec;

/// Rejects an empty (or all-whitespace) Kubernetes version; otherwise
/// returns it trimmed of surrounding whitespace, mirroring
/// [`require_component_name`]'s trim-and-return shape so a padded
/// Kubernetes version (`" 1.29.4 "`) doesn't keep its padding in
/// [`crate::ResolvedEnvironment::kubernetes`] the way a padded component
/// name wouldn't either.
///
/// `field` is the dotted locator of the enclosing environment (`baseline`
/// or `candidate`), used to build the error's locator prefix.
pub(crate) fn kubernetes_version(
    field: &str,
    version: &str,
    path: &Path,
) -> Result<String, SpecError> {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        return Err(SpecError::validation(
            path,
            format_args!("{field}.kubernetes"),
            "must not be empty",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Rejects an empty (or all-whitespace) image reference and returns the
/// list trimmed.
///
/// The check is deliberately shallow — non-empty, and nothing else. A
/// container image reference's grammar is the registry's business, not
/// this crate's, and the one failure mode worth catching here is the one
/// that would otherwise reach a cluster backend as an empty argv element:
/// a side-load command handed `""` either errors obscurely or, worse,
/// succeeds having loaded nothing. Duplicates are *not* rejected: loading
/// the same image twice is a no-op the backend absorbs, and there is no
/// reading under which a repeated entry is a mistake worth failing a run
/// for.
///
/// `field` is the dotted locator of the enclosing environment (`baseline`
/// or `candidate`).
pub(crate) fn environment_images(
    field: &str,
    images: &[String],
    path: &Path,
) -> Result<Vec<String>, SpecError> {
    images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let trimmed = image.trim();
            if trimmed.is_empty() {
                return Err(SpecError::validation(
                    path,
                    format_args!("{field}.images[{index}]"),
                    "must not be empty",
                ));
            }
            Ok(trimmed.to_owned())
        })
        .collect()
}

/// Requires a component to have a non-empty resolved name, returning it.
///
/// A component's `name` field is optional in YAML (a later task's recipe
/// system may eventually derive one), but resolution has no recipe system
/// to fall back on yet, so a name is required for now — without one,
/// [`unique_component_names`] would have nothing meaningful to compare.
pub(crate) fn require_component_name(
    field: &str,
    index: usize,
    name: Option<&str>,
    path: &Path,
) -> Result<String, SpecError> {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => Ok(name.to_owned()),
        _ => Err(SpecError::validation(
            path,
            format_args!("{field}.components[{index}]"),
            "name must not be empty (recipe-derived naming is not yet implemented)",
        )),
    }
}

/// Rejects a duplicate component name within one environment's resolved
/// component list.
///
/// Scoped to a single environment deliberately: baseline and candidate
/// are expected to install components under the *same* name (that is how
/// they are paired for comparison), so checking across both environments
/// would reject the tool's own normal use case.
pub(crate) fn unique_component_names(
    field: &str,
    components: &[ResolvedComponent],
    path: &Path,
) -> Result<(), SpecError> {
    let mut seen = std::collections::BTreeSet::new();
    for component in components {
        if !seen.insert(component.name.as_str()) {
            return Err(SpecError::validation(
                path,
                format_args!("{field}.components"),
                format_args!("duplicate component name {:?}", component.name),
            ));
        }
    }
    Ok(())
}

/// Rejects an empty fixture include list.
pub(crate) fn fixture_include_nonempty(include: &[String], path: &Path) -> Result<(), SpecError> {
    if include.is_empty() {
        return Err(SpecError::validation(
            path,
            "fixtures.include",
            "must not be empty",
        ));
    }
    Ok(())
}

/// Rejects an empty manifests-install path list.
///
/// `locator` is the dotted locator of the enclosing component (for
/// example `baseline.components[0]`), used to build the error's locator
/// prefix -- mirrors [`fixture_include_nonempty`]'s shape (an empty,
/// resolvable-but-meaningless list) applied to a component's `install`
/// block instead of the top-level `fixtures` block. A manifests install
/// naming no files would otherwise resolve cleanly, apply zero
/// `kubectl` invocations, and report success: an install that installs
/// nothing and calls that success is exactly the kind of quiet no-op
/// this project exists to catch.
pub(crate) fn manifests_paths_nonempty(
    locator: &str,
    paths: &[PathBuf],
    path: &Path,
) -> Result<(), SpecError> {
    if paths.is_empty() {
        return Err(SpecError::validation(
            path,
            format_args!("{locator}.install.paths"),
            "must not be empty",
        ));
    }
    Ok(())
}

/// Requires a component to declare an install method, returning it.
///
/// [`crate::ComponentSpec::install`] is optional in YAML — a future
/// recipe-driven component (Task 2.5) may eventually omit it — but
/// resolution has no recipe system to derive an
/// [`crate::InstallMethod`] from yet, so an explicit `install` block is
/// required for now. The complementary "both Helm and manifests"
/// failure mode needs no check here: [`InstallMethodSpec`]'s internally
/// tagged, `deny_unknown_fields` shape already rejects that combination
/// while parsing, before resolution ever runs.
pub(crate) fn require_install_method<'a>(
    locator: &str,
    install: Option<&'a InstallMethodSpec>,
    path: &Path,
) -> Result<&'a InstallMethodSpec, SpecError> {
    install.ok_or_else(|| {
        SpecError::validation(
            path,
            format_args!("{locator}.install"),
            "must be set (recipe-based installation is not implemented until Task 2.5)",
        )
    })
}

/// Requires a Helm install's chart reference to be non-empty, returning
/// it trimmed.
///
/// Mirrors `require_helm_repo_url`/`require_pinned_helm_version`'s own
/// shape: `chart` is a plain required `String` in
/// [`crate::model::HelmInstallSpec`] (so it can never be entirely
/// absent), but nothing at the type level stops it from being an empty
/// string, which would otherwise sail through resolution and only fail
/// later when something actually shells out to `helm` with it.
pub(crate) fn require_helm_chart(
    locator: &str,
    chart: &str,
    path: &Path,
) -> Result<String, SpecError> {
    let trimmed = chart.trim();
    if trimmed.is_empty() {
        return Err(SpecError::validation(
            path,
            format_args!("{locator}.install.chart"),
            "must not be empty",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Requires a Helm install's repository URL to be set, returning it
/// trimmed.
///
/// Local path and `oci://` chart references remain syntactically valid in
/// [`crate::model::HelmInstallSpec::chart`], but resolution has no way to
/// act on them without a registered repository, so `repo` is required
/// for every Helm install today.
pub(crate) fn require_helm_repo_url(
    locator: &str,
    repo: Option<&str>,
    path: &Path,
) -> Result<String, SpecError> {
    match repo.map(str::trim) {
        Some(url) if !url.is_empty() => Ok(url.to_owned()),
        _ => Err(SpecError::validation(
            path,
            format_args!("{locator}.install.repo"),
            "must be set (local path and oci:// chart references are not yet supported)",
        )),
    }
}

/// Requires a Helm install's chart version to be an explicit, exact pin,
/// returning it trimmed.
///
/// "Pinned" means: an optional `v`/`V` prefix, then exactly three
/// dot-separated, purely numeric `MAJOR.MINOR.PATCH` segments, then an
/// optional `-prerelease` and/or `+build` suffix (`SemVer`'s own grammar
/// for those two trailing parts; their content is not policed beyond
/// "non-empty"). Anything else is rejected as "floating": an empty or
/// omitted version, the literal word `latest` (rejected simply by not
/// matching the numeric grammar — no separate case-insensitive check is
/// needed), a range (`^3.9`, `>=3.9`, `~1.2.3`), a wildcard (`1.2.x`,
/// `1.2.*`), or a partial version — a bare major (`3`) or bare
/// major.minor (`3.9`) — because Masterminds/semver, the constraint
/// library Helm's own `--version` flag uses, expands every one of those
/// into a range rather than an exact version, so the chart actually
/// installed could differ between a baseline run and a candidate run (or
/// between two runs of the same side) even though the configuration
/// never changed. A chart's own `Chart.yaml` `kubeVersion` constraint is
/// a completely different, chart-authored compatibility statement and is
/// not policed here.
pub(crate) fn require_pinned_helm_version(
    locator: &str,
    version: Option<&str>,
    path: &Path,
) -> Result<String, SpecError> {
    let trimmed = version.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return Err(SpecError::validation(
            path,
            format_args!("{locator}.install.version"),
            "must be set explicitly (an empty or omitted chart version is not reproducible)",
        ));
    }
    if !is_pinned_semver(trimmed) {
        return Err(SpecError::validation(
            path,
            format_args!("{locator}.install.version"),
            format_args!(
                "{trimmed:?} is not an exact pinned version — ranges, \"latest\", wildcards, \
                 and partial versions (a bare major or major.minor) are not reproducible; use a \
                 full major.minor.patch such as \"1.14.4\" or \"v1.14.4\""
            ),
        ));
    }
    Ok(trimmed.to_owned())
}

/// See [`require_pinned_helm_version`]'s documentation for the exact
/// grammar this accepts.
///
/// Deliberately an *allow-list* (accept only what looks like a real
/// pinned version) rather than a *deny-list* of "floating-looking"
/// characters: every one of Helm's own floating forms fails this
/// grammar, so there is nothing further to deny-list separately.
fn is_pinned_semver(version: &str) -> bool {
    let core = version.strip_prefix(['v', 'V']).unwrap_or(version);

    // Split off the optional `+build` suffix first (SemVer requires it
    // to be the last component), then the optional `-prerelease` suffix
    // from what remains; each, if introduced by its separator, must be
    // non-empty.
    let (core, build) = match core.split_once('+') {
        Some((core, build)) => (core, Some(build)),
        None => (core, None),
    };
    if build.is_some_and(str::is_empty) {
        return false;
    }

    let (release, prerelease) = match core.split_once('-') {
        Some((release, prerelease)) => (release, Some(prerelease)),
        None => (core, None),
    };
    if prerelease.is_some_and(str::is_empty) {
        return false;
    }

    let segments: Vec<&str> = release.split('.').collect();
    segments.len() == 3
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()))
}

/// Requires a resolved component version: `version` (the component's own
/// top-level, trimmed override) if set and non-empty, otherwise
/// `fallback` (the install method's own version, when the install method
/// provides an unambiguous one — only a pinned Helm chart version
/// qualifies; a manifests install has none).
pub(crate) fn require_component_version(
    locator: &str,
    version: Option<&str>,
    fallback: Option<&str>,
    path: &Path,
) -> Result<String, SpecError> {
    if let Some(version) = version.map(str::trim)
        && !version.is_empty()
    {
        return Ok(version.to_owned());
    }
    if let Some(fallback) = fallback {
        return Ok(fallback.to_owned());
    }
    Err(SpecError::validation(
        path,
        format_args!("{locator}.version"),
        "must be set explicitly (no install method here provides an implicit version)",
    ))
}

/// Validates a [`crate::GatewaySuiteSpec`]'s non-path content (ROADMAP
/// Task 6.1 Steps 1-2).
///
/// Everything here is a *strict config* rule: it rejects a document that
/// parsed cleanly but describes a suite no run could ever satisfy — an
/// empty manifest list, two contracts sharing an id, a Gateway named by
/// an empty string, a method no conformant Gateway can match, or a
/// status code that cannot appear on the wire. Every one of these would
/// otherwise surface much later, on a real cluster, after two `kind`
/// clusters had already been created.
///
/// [`crate::resolve_lab`] resolves [`crate::GatewaySuiteSpec::manifests`]
/// separately (paths are the one thing resolution rewrites); this
/// function never touches them beyond the emptiness check.
pub(crate) fn gateway_suite(
    gateway: &crate::model::GatewaySuiteSpec,
    path: &Path,
) -> Result<(), SpecError> {
    if gateway.manifests.is_empty() {
        return Err(SpecError::validation(
            path,
            "gateway.manifests",
            "must not be empty",
        ));
    }
    if gateway.reconciliation_timeout.is_zero() {
        return Err(SpecError::validation(
            path,
            "gateway.reconciliationTimeout",
            "must be greater than zero (a zero timeout can never observe the two-poll \
             stability window reconciliation requires)",
        ));
    }
    if gateway.routes.is_empty() {
        return Err(SpecError::validation(
            path,
            "gateway.routes",
            "must not be empty",
        ));
    }

    if let Some(endpoint) = &gateway.gateway_endpoint {
        // Delegated rather than re-checked: `resolve_gateway_endpoint`
        // is the one validator for this vocabulary, shared with the
        // recipe loader, and it proves each substitutable field's
        // placeholders by running the real substitution. See that
        // function's "Why this lives here" section.
        crate::component::resolve_gateway_endpoint(endpoint).map_err(|(locator, message)| {
            SpecError::validation(
                path,
                format_args!("gateway.gatewayEndpoint.{locator}"),
                message,
            )
        })?;
    }

    let mut seen_ids = std::collections::BTreeSet::new();
    for (index, route) in gateway.routes.iter().enumerate() {
        let locator = format!("gateway.routes[{index}]");
        let id = require_non_empty(&locator, "id", &route.id, path)?;
        if !seen_ids.insert(id.clone()) {
            return Err(SpecError::validation(
                path,
                format_args!("{locator}.id"),
                format_args!("duplicate route contract id {id:?}"),
            ));
        }
        require_non_empty(&locator, "gatewayNamespace", &route.gateway_namespace, path)?;
        require_non_empty(&locator, "gatewayName", &route.gateway_name, path)?;
        require_non_empty(&locator, "routeNamespace", &route.route_namespace, path)?;
        require_non_empty(&locator, "routeName", &route.route_name, path)?;
        if let Some(listener) = &route.listener_name {
            // Present-but-empty is rejected rather than treated as
            // absent: `listenerName: ""` reads as a deliberate narrowing
            // to a listener whose name is the empty string, which Gateway
            // API's own `sectionName` (a required, non-empty
            // `SectionName`) can never be. Silently widening it back to
            // "any listener" would make the two spellings mean the same
            // thing while looking different.
            require_non_empty(&locator, "listenerName", listener, path)?;
        }

        for (probe_index, probe) in route.probes.iter().enumerate() {
            let locator = format!("{locator}.probes[{probe_index}]");
            require_non_empty(&locator, "host", &probe.host, path)?;
            gateway_probe_path(&locator, &probe.path, path)?;
            gateway_probe_method(&locator, &probe.method, path)?;
            gateway_probe_status(&locator, probe.expected_status, path)?;
            if let Some(backend) = &probe.expected_backend {
                // Same reasoning as `listenerName` above: an empty
                // `expectedBackend` would silently mean "any backend",
                // which is already spelled by omitting the field.
                require_non_empty(&locator, "expectedBackend", backend, path)?;
            }
        }
    }
    Ok(())
}

/// Requires `value` to be non-empty once trimmed, returning it trimmed.
///
/// A shared helper for [`gateway_suite`]'s many required string fields,
/// mirroring [`require_component_name`]'s trim-and-return shape so a
/// padded contract id (`" web "`) does not keep its padding in the value
/// later used to correlate baseline and candidate results.
fn require_non_empty(
    locator: &str,
    field: &str,
    value: &str,
    path: &Path,
) -> Result<String, SpecError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(SpecError::validation(
            path,
            format_args!("{locator}.{field}"),
            "must not be empty",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Rejects a probe path that does not begin with `/`.
///
/// An `HTTPRoute`'s own `matches[].path.value` is required to start with
/// `/` (Gateway API validates that on the CRD), and an HTTP
/// origin-form request target is `absolute-path [ "?" query ]` — so a
/// probe path without a leading slash could not be sent as written and
/// could not match any route.
fn gateway_probe_path(locator: &str, probe_path: &str, path: &Path) -> Result<(), SpecError> {
    if !probe_path.starts_with('/') {
        return Err(SpecError::validation(
            path,
            format_args!("{locator}.path"),
            format_args!("must start with \"/\", found {probe_path:?}"),
        ));
    }
    Ok(())
}

/// Rejects a probe method outside [`crate::model::ALLOWED_HTTP_METHODS`].
///
/// See that constant for the list's provenance (Gateway API v1's own
/// `HTTPMethod` enumeration) and for why the comparison is
/// case-sensitive.
fn gateway_probe_method(locator: &str, method: &str, path: &Path) -> Result<(), SpecError> {
    if crate::model::ALLOWED_HTTP_METHODS.contains(&method) {
        return Ok(());
    }
    Err(SpecError::validation(
        path,
        format_args!("{locator}.method"),
        format_args!(
            "{method:?} is not an HTTP method a Gateway API HTTPRoute can match; use one of \
             {} (uppercase — HTTP methods are case-sensitive)",
            crate::model::ALLOWED_HTTP_METHODS.join(", ")
        ),
    ))
}

/// Rejects a probe status code outside `100..=599`. See
/// [`crate::model::is_valid_http_status`] for the range's source and for
/// why unassigned-but-in-range codes stay legal.
fn gateway_probe_status(locator: &str, status: u16, path: &Path) -> Result<(), SpecError> {
    if crate::model::is_valid_http_status(status) {
        return Ok(());
    }
    Err(SpecError::validation(
        path,
        format_args!("{locator}.expectedStatus"),
        format_args!("{status} is not an HTTP status code; must be in 100..=599"),
    ))
}

/// Rejects a [`crate::LabSpec`] whose `apiVersion`/`kind` do not match the
/// only values [`crate::load_lab`] accepts.
pub(crate) fn api_version_and_kind(
    api_version: &str,
    kind: &str,
    path: &Path,
) -> Result<(), SpecError> {
    if api_version != crate::model::API_VERSION {
        return Err(SpecError::validation(
            path,
            "apiVersion",
            format_args!(
                "must be {:?}, found {api_version:?}",
                crate::model::API_VERSION
            ),
        ));
    }
    if kind != crate::model::KIND {
        return Err(SpecError::validation(
            path,
            "kind",
            format_args!("must be {:?}, found {kind:?}", crate::model::KIND),
        ));
    }
    Ok(())
}
