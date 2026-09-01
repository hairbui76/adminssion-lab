//! Tests for the standalone HTML artifact.
//!
//! There is no golden file here, deliberately. The HTML is presentation:
//! its exact markup is expected to change as the page is improved, and a
//! byte-for-byte golden would turn every styling tweak into a
//! regenerate-the-fixture chore that trains reviewers to accept diffs
//! without reading them. What is pinned instead are the properties that
//! must never change no matter how the page looks: it loads nothing from
//! the network, it escapes every string it did not write itself, it
//! shows regressions without an interaction, and it never presents an
//! unattributed or inferred divergence as an observation.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use admissionlab_policy::Severity;
use admissionlab_report::{LabResult, escape_html, render_html, write_html_report};
use support::canonical_result;

/// A `<script>` payload planted in vendor-controlled strings.
///
/// A webhook name and a rejection message are chosen on the *candidate*
/// side, which is precisely the attacker-influenced surface this tool
/// points at: the stack under test decides both.
const XSS_SENTINEL: &str = "<script>alert('admissionlab')</script>";

/// A temporary directory that removes itself when dropped.
///
/// A test holds one for as long as it uses paths underneath it. `Drop`
/// runs on a panicking assertion too, which an explicit delete at the
/// end of a test does not — that is what keeps a `cargo test` run from
/// leaving a directory per test behind in the system temp directory.
struct TempDir(PathBuf);

impl TempDir {
    /// The directory's path, valid for as long as this guard lives.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh, guaranteed-unique directory under the system temp directory.
fn unique_temp_dir(label: &str) -> TempDir {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-report-html-test-{}-{label}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    TempDir(dir)
}

/// [`canonical_result`] with [`XSS_SENTINEL`] planted in five different
/// vendor- and user-supplied strings, each reaching the page through a
/// different renderer.
fn xss_result() -> LabResult {
    let mut result = canonical_result();

    let admission = result.fixtures[0]
        .admission
        .as_mut()
        .expect("the critical fixture has an admission comparison");
    XSS_SENTINEL.clone_into(&mut admission.candidate.trace.invocations[0].webhook);
    admission.candidate.warnings.push(XSS_SENTINEL.to_owned());
    XSS_SENTINEL.clone_into(
        &mut admission
            .first_divergence
            .as_mut()
            .expect("the critical fixture has a divergence")
            .explanation,
    );

    result.fixtures[0].changes[0].change.subject = Some(XSS_SENTINEL.to_owned());
    result.policy.changes[0].change.subject = Some(XSS_SENTINEL.to_owned());
    XSS_SENTINEL.clone_into(&mut result.policy.stale_expectations[0].reason);
    XSS_SENTINEL.clone_into(&mut result.diagnostics[0].message);

    result
}

/// The page must open with no server and no network.
///
/// # What is checked, and what stopped being checked at Task 8.8
///
/// Every construct below is one a browser *fetches through*: a script or
/// stylesheet element, a `src`/`href` attribute, a CSS `@import` or
/// `url(...)`, or an embedded document. Absent all of them, the page has
/// no way to reach the network, which is the property this test exists
/// to hold.
///
/// The list used to also forbid the bare strings `http://`, `https://`
/// and `//cdn`. Those were proxies for the constructs above and are
/// strictly weaker than them -- a URI inside escaped text content is
/// inert, because nothing fetches it -- and ROADMAP Task 8.8 made them
/// wrong as well as redundant: a migration finding's `detail` is written
/// by `admissionlab_gateway::describe_probe_request`, whose whole
/// rendering of a probe is `GET http://<the user's own host><path>`. A
/// report of a real migration therefore *contains* a URI, as data, and
/// must. The fetch vectors are still all forbidden, and
/// [`a_script_tag_in_a_vendor_string_is_escaped_never_raw`] separately
/// proves that data cannot become one.
#[test]
fn the_page_loads_nothing_from_outside_itself() {
    let page = render_html(&canonical_result());
    let lowered = page.to_lowercase();

    for forbidden in [
        "<script", "<link", "src=", "href=", "@import", "url(", "<iframe", "<object", "<embed",
    ] {
        assert!(
            !lowered.contains(forbidden),
            "the page must open with no server and no network, but it contains `{forbidden}`"
        );
    }
}

#[test]
fn all_styling_is_inline_in_one_style_block() {
    let page = render_html(&canonical_result());

    assert_eq!(
        page.matches("<style>").count(),
        1,
        "exactly one inline stylesheet, and no external one"
    );
    assert!(page.contains("</style>"));
}

#[test]
fn the_page_contains_no_javascript() {
    let page = render_html(&canonical_result());
    let lowered = page.to_lowercase();

    assert!(!lowered.contains("javascript:"));
    assert!(
        !lowered.contains(" onclick") && !lowered.contains(" onload"),
        "the drill-down is <details>/<summary>; no event handler is needed"
    );
    assert!(
        page.contains("<details"),
        "the drill-down must actually use <details>, or the previous assertions prove nothing"
    );
}

#[test]
fn no_placeholder_is_left_unsubstituted() {
    let page = render_html(&canonical_result());

    assert!(
        !page.contains("{{"),
        "an unsubstituted placeholder means a marker in the template has no value"
    );
}

#[test]
fn a_script_tag_in_a_vendor_string_is_escaped_never_raw() {
    let page = render_html(&xss_result());

    assert!(
        !page.contains(XSS_SENTINEL),
        "the raw sentinel reached the page; it would execute in a browser"
    );
    assert!(
        !page.to_lowercase().contains("<script"),
        "no `<script` may survive in any form"
    );
    assert!(
        page.contains("&lt;script&gt;alert(&#39;admissionlab&#39;)&lt;/script&gt;"),
        "the sentinel must still be *visible* as text -- escaping must not drop it"
    );
}

#[test]
fn every_planted_sentinel_appears_escaped() {
    let page = render_html(&xss_result());
    let escaped = escape_html(XSS_SENTINEL);

    assert!(
        page.matches(escaped.as_str()).count() >= 5,
        "each of the five planted locations must render the sentinel as escaped text; \
         found {} occurrence(s)",
        page.matches(escaped.as_str()).count()
    );
}

#[test]
fn escaping_covers_all_five_characters() {
    assert_eq!(
        escape_html(r#"a & b < c > d " e ' f"#),
        "a &amp; b &lt; c &gt; d &quot; e &#39; f"
    );
    assert_eq!(escape_html("plain"), "plain");
}

#[test]
fn critical_and_warning_fixtures_are_open_and_quiet_ones_are_not() {
    let page = render_html(&canonical_result());

    assert!(
        page.contains("<details class=\"fixture b-critical\" open>"),
        "a regression must not be behind an interaction"
    );
    assert!(
        page.contains("<details class=\"fixture b-identical\">"),
        "a quiet fixture starts collapsed"
    );
    assert!(page.contains("<details class=\"fixture b-inconclusive\">"));
}

#[test]
fn a_warning_fixture_is_open_too() {
    let mut result = canonical_result();
    let mut warning = result.fixtures[0].changes[0].clone();
    warning.severity = Severity::Warning;
    warning.expected = false;
    result.fixtures[3].changes.push(warning);

    let page = render_html(&result);

    assert!(page.contains("<details class=\"fixture b-warnings\" open>"));
}

#[test]
fn every_summary_count_is_on_the_page() {
    let page = render_html(&canonical_result());

    for label in [
        "identical",
        "expected",
        "warnings",
        "critical",
        "inconclusive",
    ] {
        assert!(
            page.contains(&format!("<span class=\"label\">{label}</span>")),
            "the `{label}` count is missing"
        );
    }
    assert!(page.contains("fixtures total"));
}

#[test]
fn a_fixture_panel_carries_the_full_drill_down() {
    let page = render_html(&canonical_result());

    for heading in [
        "<h3>Decision</h3>",
        "<h3>First divergence</h3>",
        "<h3>Changes</h3>",
        "<h3>Webhook trace</h3>",
        "<h3>Raw evidence</h3>",
    ] {
        assert!(
            page.contains(heading),
            "the drill-down is missing {heading}"
        );
    }
    // The decision pair, a classified change with its severity and
    // expectation state, and a trace invocation all reach the page.
    assert!(page.contains("<code>accepted</code>"));
    assert!(page.contains("<span class=\"badge badge-critical\">critical</span>"));
    assert!(page.contains("<span class=\"badge badge-expected\">expected</span>"));
    assert!(page.contains("inject.example.com"));
    assert!(page.contains("Both sides' final objects are captured above"));
}

#[test]
fn an_unknown_confidence_divergence_is_spelled_out() {
    let page = render_html(&canonical_result());

    assert!(
        page.contains("unknown &mdash; a difference exists but the captured evidence does not"),
        "an unlocated divergence must never read like an observation"
    );
}

#[test]
fn a_missing_attribution_says_so_rather_than_staying_silent() {
    let mut result = canonical_result();
    result.fixtures[0].changes[0].change.origin = None;
    result.fixtures[0]
        .admission
        .as_mut()
        .expect("the critical fixture has an admission comparison")
        .first_divergence = None;

    let page = render_html(&result);

    assert!(
        page.contains("never that the two sides agreed"),
        "silence in the divergence slot reads as `there was no divergence`"
    );
}

#[test]
fn an_unmeasured_latency_is_not_rendered_as_zero() {
    // The identical fixture's single invocation has `latency: None` and
    // `mutated: Some(false)`.
    let page = render_html(&canonical_result());

    assert!(page.contains("not measured"));
    assert!(
        !page.contains("<td>0 ms</td>"),
        "a fabricated zero would read as `instantaneous`"
    );
}

#[test]
fn an_entry_with_no_evidence_at_all_says_nothing_was_established() {
    let mut result = canonical_result();
    result.fixtures[3].admission = None;

    let page = render_html(&result);

    // "on either side" rather than "admission evidence": since ROADMAP
    // Task 6.11 an entry carries admission *or* Gateway evidence, and a
    // message naming only the first would be wrong for half the entries
    // a lab with a `gateway:` section produces.
    assert!(page.contains("No evidence was captured for this entry on either side"));
}

#[test]
fn rendering_is_deterministic() {
    let result = canonical_result();

    assert_eq!(render_html(&result), render_html(&result));
}

#[test]
fn writing_produces_exactly_the_rendered_page() {
    let result = canonical_result();
    let temp_dir = unique_temp_dir("write");
    let path = temp_dir.path().join("report.html");

    write_html_report(&path, &result).expect("writing the report succeeds");

    let written = std::fs::read_to_string(&path).expect("the report was written");
    assert_eq!(written, render_html(&result));
    assert!(written.starts_with("<!DOCTYPE html>"));
}

#[test]
fn writing_leaves_no_temporary_file_behind() {
    let dir = unique_temp_dir("no-temp");
    write_html_report(&dir.path().join("report.html"), &canonical_result())
        .expect("writing succeeds");

    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read the temp dir")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    assert_eq!(entries, vec!["report.html".to_owned()]);
}
