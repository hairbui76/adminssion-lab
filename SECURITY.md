# Security Policy

## Supported versions

Admission Lab is pre-1.0. Security fixes are provided only for the latest
commit on the default branch; there are no maintained release branches yet.

## Reporting a vulnerability

Please **do not** open a public GitHub issue for a suspected security
vulnerability.

Instead, report it privately using one of the following:

- GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability)
  feature on this repository's **Security** tab, or
- opening a [GitHub Security Advisory](https://github.com/hairbui76/adminssion-lab/security/advisories/new)
  directly.

Please include:

- a description of the vulnerability and its potential impact;
- steps to reproduce it, including the Admission Lab version/commit,
  Kubernetes version, and any relevant configuration or fixtures;
- any known mitigation.

## What counts as in scope

Admission Lab creates and tears down ephemeral local Kubernetes clusters
and installs components into them. In-scope reports include (but are not
limited to):

- vulnerabilities that allow code execution beyond what the invoked
  external tools (`kind`, `kubectl`, `helm`) are explicitly asked to run;
- unsafe handling of kubeconfig material, credentials, or other secrets;
- issues that would let a fixture or recipe escape its intended scope
  (for example, reaching outside the ephemeral cluster it was applied to).

Admission Lab is safe-by-default and does not require production secrets
or production write access for its default test flow; reports assuming a
production deployment mode outside that default flow should say so
explicitly.

## Response expectations

This is a community-maintained open source project without a dedicated
security team or bug bounty program. We will acknowledge new reports as
soon as we are able to and aim to keep reporters updated as a fix is
developed. Coordinated disclosure timing is negotiated with the reporter
on a case-by-case basis.
