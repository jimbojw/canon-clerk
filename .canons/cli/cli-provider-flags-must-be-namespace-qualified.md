---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI flags configuring provider- or service-specific operational options (such as credentials, endpoints, or vendor settings) MUST be namespace-qualified with the provider name (e.g. `--<provider>-<option>`) rather than adopting generic root names.

Rationale: Adhering to multi-provider SDK conventions (e.g. OpenTelemetry, AWS CLI) and clig.dev §Arguments and flags, generic names like `--api-key` presume a single vendor, forcing breaking deprecations when additional integrations arrive.

**Remediation:** Prefix provider-specific flags and environment variable fallbacks with the service slug (e.g. `--gemini-api-key` / `GEMINI_API_KEY`, `--github-token` / `GITHUB_TOKEN`), reserving generic flags strictly for common, vendor-agnostic abstractions.
