---
inspect:
  - diff
tags:
  - internal
  - architecture
  - action
---
The GitHub Action runner (`@canon-clerk/action`) MUST act strictly as an integration adapter that maps workflow inputs, environment variables, and GitHub Actions step annotations to and from `@canon-clerk/core`; it MUST NOT implement core domain evaluation, canon cascade logic, or path filtering algorithms directly.

Rationale: Conforming to GitHub's thin adapter architecture and ISO/IEC 25010 testability, action runners must purely adapt workflow contexts; embedding domain logic creates behavioral divergence between local pre-push validation and remote CI gates.

**Remediation:** Keep `packages/action` minimal by parsing inputs (via `@actions/core`), invoking domain methods exposed by `@canon-clerk/core`, and translating the structured results into GitHub Action summaries, outputs, and annotations.
