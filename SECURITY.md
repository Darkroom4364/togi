# Security Policy

## Supported Versions

togi is still pre-1.0. Security fixes are applied on a best-effort basis to the
latest tagged release and the current `main` branch.

| Version | Supported |
| --- | --- |
| Latest tagged release | Yes |
| Current `main` branch | Yes |
| Older tags and maintenance branches | No |

## Reporting a Vulnerability

Do not open a public GitHub issue for an unpatched security vulnerability.

Prefer GitHub's private vulnerability reporting flow for this repository. If
private reporting is unavailable, contact the maintainer privately through
GitHub before disclosing details publicly.

Include the affected togi version or commit, operating system, reproduction
steps, expected impact, and whether the issue depends on a particular target
language, test command, or CI environment.

The maintainer will triage the report, work on a fix, and coordinate public
disclosure after a patch or mitigation is available when possible.

## Security Model

togi is a local and CI command runner. It parses repository files, creates
isolated mutation workspaces, and executes repository-defined build and test
commands.

Those workspace copies, timeouts, and descendant-process cleanup are operational
guardrails for correctness and cleanup. They are not a security sandbox.

togi does not currently restrict filesystem or network access for spawned
commands beyond whatever the host operating system, container, or CI runner
already enforces. Treat the target repository and its configured test commands
as trusted code. Running less-trusted repositories directly on the host is out
of scope for the current security model.

If you need to evaluate less-trusted repositories, run togi inside a separate
container, VM, or similarly restricted environment with least-privilege
credentials and explicit filesystem/network boundaries.
