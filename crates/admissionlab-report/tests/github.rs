//! Golden, escaping, and size tests for the GitHub job summary.
//!
//! The golden is inline for the same reason `tests/terminal.rs`'s is: it
//! is the shape of one function's output, small enough to read in a diff,
//! and having the expected text next to the assertion is what makes a
//! reviewer notice when a rendering change quietly drops a qualifier or a
//! cap.
//!
//! Two of the tests here are not about looks at all. The hostile-string
//! tests plant the payloads a compromised or merely careless webhook can
//! put in a name -- a `|` that would silently re-align an entire table,
//! and HTML that GitHub *would* render in a step summary -- and the size
//! test proves the documented byte budget holds for a run far larger than
//! any this project's own examples produce.

mod support;

use admissionlab_policy::{PolicyDisposition, Severity};
use admissionlab_report::{
    LabResult, MAX_CELL_CHARS, MAX_LISTED_FINDINGS, SUMMARY_BYTE_BUDGET, escape_markdown,
    render_github_summary, write_github_summary,
};
use support::canonical_result;

/// The exact job summary for [`canonical_result`].
///
/// Covers the verdict with its exit-code meaning, the run identity line,
/// all five bucket counts plus the total, a critical finding with its
/// subject, kind, object path and `observed` divergence including both
/// webhook sides, the Gateway traffic findings in the warnings section,
/// and the artifact pointers.
const GOLDEN: &str = r"## Admission Lab: FAIL

At least one unexpected critical difference. `admissionlab test` exits 1.

Run beta-demo-run — result schema admissionlab.io/result/v1beta1 (frozen; additive changes only).

### Fixtures

| Bucket | Fixtures |
| --- | ---: |
| identical | 1 |
| expected | 1 |
| warnings | 1 |
| critical | 1 |
| inconclusive | 1 |
| **total** | **5** |

### Critical findings (1)

| Fixture | Subject | Change | Evidence |
| --- | --- | --- | --- |
| deployment-sidecar | istio-proxy | container\_added at /spec/template/spec/containers/1 | observed: the container appears in inject.example.com's candidate patch (baseline none -> candidate inject.example.com) |

### Warnings (2)

| Fixture | Subject | Change | Evidence |
| --- | --- | --- | --- |
| echo-route-contract | echo-route | traffic\_status\_changed | baseline HTTP 200 from echo-v1 -&gt; candidate HTTP 503 from echo-v1 |
| echo-route-contract | echo-route | traffic\_status\_changed | baseline HTTP 204 from echo-v1 -&gt; candidate answered nothing |

### Full evidence

This summary lists at most 10 findings per severity and carries no webhook traces, patches, or object bodies. Everything is in the artifacts uploaded with this workflow run:

- `result.json` — the machine-readable result: every fixture, every graded change, and both sides' captured admission outcomes.
- `report.html` — the standalone report page: per-fixture drill-down with the full webhook trace and every patch.
- run manifest — what this run actually ran (tool versions, images, and configuration digests), for reproducing it.
";

#[test]
fn the_canonical_summary_matches_the_golden() {
    let rendered = render_github_summary(&canonical_result());

    assert_eq!(rendered, GOLDEN, "rendered summary was:\n{rendered}");
}

#[test]
fn rendering_is_deterministic() {
    let result = canonical_result();

    assert_eq!(
        render_github_summary(&result),
        render_github_summary(&result)
    );
}

#[test]
fn each_disposition_states_its_own_exit_code() {
    for (disposition, status, code) in [
        (PolicyDisposition::Pass, "PASS", "exits 0"),
        (PolicyDisposition::Warn, "WARN", "exits 0"),
        (PolicyDisposition::Fail, "FAIL", "exits 1"),
    ] {
        let mut result = canonical_result();
        result.policy.disposition = disposition;

        let rendered = render_github_summary(&result);

        assert!(
            rendered.starts_with(&format!("## Admission Lab: {status}\n")),
            "{disposition:?} must lead with {status}; summary was:\n{rendered}"
        );
        assert!(
            rendered.contains(&format!("`admissionlab test` {code}")),
            "a reader must not have to guess whether {status} fails the job; summary was:\n\
             {rendered}"
        );
    }
}

#[test]
fn the_status_word_carries_no_emoji() {
    let rendered = render_github_summary(&canonical_result());

    // Punctuation above ASCII is fine (the em dash and the truncation
    // ellipsis are both deliberate); pictographs and their variation
    // selectors are not.
    let pictograph = rendered.chars().find(|character| {
        matches!(u32::from(*character),
            0x2190..=0x21FF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE0F | 0x1F000..=0x1FAFF)
    });

    assert!(
        pictograph.is_none(),
        "the verdict is spelled out in letters, not signalled with {pictograph:?}, which reads \
         as nothing in an email notification or to a screen reader; summary was:\n{rendered}"
    );
}

#[test]
fn a_pipe_in_a_webhook_name_cannot_break_the_table() {
    let mut result = canonical_result();
    let origin = result.policy.changes[0]
        .change
        .origin
        .as_mut()
        .expect("the critical change carries its own attribution");
    origin.candidate_webhook = Some("evil|name.example.com".to_owned());
    result.policy.changes[0].change.subject = Some("pipe|subject".to_owned());

    let rendered = render_github_summary(&result);
    let widths: Vec<usize> = rendered
        .lines()
        .filter(|line| line.starts_with("| ") && !line.starts_with("| ---"))
        .map(cell_count)
        .collect();

    assert!(
        rendered.contains(r"evil\|name.example.com"),
        "the pipe must survive as literal text; summary was:\n{rendered}"
    );
    assert!(
        widths.iter().all(|width| *width == 2 || *width == 4),
        "every row must keep its declared cell count (2 for the bucket table, 4 for a finding \
         table); widths were {widths:?} in:\n{rendered}"
    );
}

#[test]
fn html_in_a_vendor_string_arrives_as_text() {
    let mut result = canonical_result();
    let origin = result.policy.changes[0]
        .change
        .origin
        .as_mut()
        .expect("the critical change carries its own attribution");
    origin.candidate_webhook = Some("<img src=x onerror=alert(1)>".to_owned());
    origin.explanation = "<script>alert('xss')</script>".to_owned();

    let rendered = render_github_summary(&result);

    assert!(
        !rendered.contains("<script>") && !rendered.contains("<img "),
        "GitHub renders HTML in step summaries, so a webhook name must never reach one as \
         markup; summary was:\n{rendered}"
    );
    assert!(
        rendered.contains("&lt;script&gt;alert('xss')&lt;/script&gt;"),
        "and it must still be readable as the text it is; summary was:\n{rendered}"
    );
    assert!(
        rendered.contains("&lt;img src=x onerror=alert(1)&gt;"),
        "summary was:\n{rendered}"
    );
}

#[test]
fn a_newline_in_a_vendor_string_cannot_end_the_table() {
    let mut result = canonical_result();
    result.policy.changes[0].change.subject = Some("first\nsecond\tthird".to_owned());

    let rendered = render_github_summary(&result);

    assert!(
        rendered.contains("first second third"),
        "control characters flatten to spaces so a cell stays one line; summary was:\n{rendered}"
    );
}

#[test]
fn escape_markdown_covers_both_layers_without_double_escaping() {
    assert_eq!(escape_markdown("a|b"), r"a\|b");
    assert_eq!(
        escape_markdown("*bold* _under_ `code`"),
        r"\*bold\* \_under\_ \`code\`"
    );
    assert_eq!(escape_markdown("[link](url)"), r"\[link\](url)");
    assert_eq!(escape_markdown("a & b"), "a &amp; b");
    assert_eq!(escape_markdown("<b>"), "&lt;b&gt;");
    assert_eq!(
        escape_markdown(r"back\slash"),
        r"back\\slash",
        "a literal backslash must not be able to escape the character after it"
    );
    assert_eq!(
        escape_markdown("&amp;"),
        "&amp;amp;",
        "an ampersand is entity-escaped once and never re-scanned"
    );
    assert_eq!(
        escape_markdown("line\r\nbreak\u{0}"),
        "line  break ",
        "every control character becomes exactly one space"
    );
    assert_eq!(
        escape_markdown("plain text 1.2.3-rc1/path"),
        "plain text 1.2.3-rc1/path",
        "characters that cannot start an inline construct are left alone, because a summary \
         full of backslashes is a summary nobody reads"
    );
}

#[test]
fn an_over_long_field_is_truncated_with_an_ellipsis() {
    let mut result = canonical_result();
    result.policy.changes[0].change.subject = Some("x".repeat(MAX_CELL_CHARS + 50));

    let rendered = render_github_summary(&result);

    assert!(
        rendered.contains(&format!("{}…", "x".repeat(MAX_CELL_CHARS))),
        "summary was:\n{rendered}"
    );
    assert!(
        !rendered.contains(&"x".repeat(MAX_CELL_CHARS + 1)),
        "one unbounded vendor string must not be able to fill the whole summary"
    );
}

#[test]
fn five_hundred_findings_stay_within_the_documented_budget() {
    let rendered = render_github_summary(&many_critical_findings(500));
    let rows = rendered
        .lines()
        .filter(|line| line.starts_with("| deployment-sidecar |"))
        .count();

    assert_eq!(
        rows, MAX_LISTED_FINDINGS,
        "the table is capped structurally, not by the reader's patience; summary was:\n{rendered}"
    );
    assert!(
        rendered.contains("### Critical findings (500)"),
        "the heading always carries the complete count; summary was:\n{rendered}"
    );
    assert!(
        rendered.contains("and 490 more critical findings"),
        "a capped table must say how much it did not show; summary was:\n{rendered}"
    );
    assert!(
        rendered.len() < SUMMARY_BYTE_BUDGET,
        "GitHub truncates a step summary at 1 MiB; this one was {} bytes",
        rendered.len()
    );
}

#[test]
fn a_hostile_run_of_findings_still_fits_the_budget() {
    // Every vendor-derived field is both over-long and made entirely of
    // characters whose escaping expands them, which is the worst case
    // the budget arithmetic is derived from.
    let mut result = many_critical_findings(500);
    for classified in &mut result.policy.changes {
        classified.change.subject = Some("&".repeat(4000));
        classified.change.object_path = Some("<".repeat(4000));
        let origin = classified
            .change
            .origin
            .as_mut()
            .expect("every generated finding carries an attribution");
        origin.explanation = ">".repeat(4000);
        origin.baseline_webhook = Some("|".repeat(4000));
        origin.candidate_webhook = Some("`".repeat(4000));
    }

    let rendered = render_github_summary(&result);

    assert!(
        rendered.len() < SUMMARY_BYTE_BUDGET,
        "the budget must hold for strings this project did not produce; summary was {} bytes",
        rendered.len()
    );
}

#[test]
fn no_trace_or_patch_detail_reaches_the_summary() {
    let rendered = render_github_summary(&canonical_result());

    assert!(
        !rendered.contains("registry.example.com/proxy:1.27.0"),
        "a patched image body belongs in report.html, not in a job summary; summary was:\n\
         {rendered}"
    );
    assert!(
        !rendered.contains("sidecar-injector"),
        "webhook configurations and component inventories are artifact detail; summary was:\n\
         {rendered}"
    );
    assert!(
        rendered.contains("`result.json`")
            && rendered.contains("`report.html`")
            && rendered.contains("run manifest"),
        "but the summary must name where that detail is; summary was:\n{rendered}"
    );
}

#[test]
fn expected_changes_are_counted_but_not_listed() {
    let rendered = render_github_summary(&canonical_result());

    assert!(
        !rendered.contains("image_changed") && !rendered.contains(r"image\_changed"),
        "an expected change is not a finding"
    );
    assert!(rendered.contains("| expected | 1 |"), "but it is counted");
}

#[test]
fn an_unattributed_finding_says_so_rather_than_rendering_an_empty_cell() {
    let mut result = canonical_result();
    result.policy.changes[0].change.origin = None;
    result.fixtures[0]
        .admission
        .as_mut()
        .expect("the critical fixture has an admission comparison")
        .first_divergence = None;

    let rendered = render_github_summary(&result);

    assert!(
        rendered.contains("| not attributed |"),
        "an empty cell would read as `the sides did not diverge`; summary was:\n{rendered}"
    );
}

#[test]
fn an_inferred_attribution_keeps_its_qualifier() {
    let mut result = canonical_result();
    let origin = result.policy.changes[0]
        .change
        .origin
        .as_mut()
        .expect("the critical change carries its own attribution");
    origin.confidence = admissionlab_diff::DivergenceConfidence::Inferred;

    let rendered = render_github_summary(&result);

    assert!(
        rendered.contains("inferred (deduced from incomplete evidence):"),
        "a deduction must never read like an observation, however tight the column; summary \
         was:\n{rendered}"
    );
}

/// [`canonical_result`] with its one critical finding cloned `count`
/// times, each with a distinct subject.
fn many_critical_findings(count: usize) -> LabResult {
    let mut result = canonical_result();
    let template = result.policy.changes[0].clone();
    result.policy.changes = (0..count)
        .map(|index| {
            let mut classified = template.clone();
            classified.severity = Severity::Critical;
            classified.expected = false;
            classified.change.subject = Some(format!("sidecar-{index}"));
            classified
        })
        .collect();
    result
}

/// How many cells a rendered table row declares.
///
/// Counts only unescaped `|` characters, which is exactly the count
/// GitHub's table parser makes -- so a row whose vendor data smuggled in
/// a live delimiter shows up here as a row of the wrong width.
fn cell_count(row: &str) -> usize {
    let mut cells: usize = 0;
    let mut escaped = false;
    for character in row.chars() {
        match character {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '|' => cells += 1,
            _ => {}
        }
    }
    // A row is `| a | b |`: one more delimiter than it has cells.
    cells.saturating_sub(1)
}

#[test]
fn writing_produces_exactly_the_rendered_summary() {
    let result = canonical_result();
    let dir = std::env::temp_dir().join(format!(
        "admissionlab-report-github-test-{}-write",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create the temp dir");
    let path = dir.join("github-summary.md");

    write_github_summary(&path, &result).expect("writing the summary succeeds");

    let written = std::fs::read_to_string(&path).expect("the summary was written");
    assert_eq!(written, render_github_summary(&result));
    // The file the Action appends to `$GITHUB_STEP_SUMMARY` must be the
    // only thing left behind: `write_atomic`'s temporary is renamed, not
    // abandoned (`tests/html.rs` makes the same check for the same
    // reason).
    let entries: Vec<String> = std::fs::read_dir(&dir)
        .expect("read the temp dir")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(entries, vec!["github-summary.md".to_owned()]);
    let _ = std::fs::remove_dir_all(&dir);
}
