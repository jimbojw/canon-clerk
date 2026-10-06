---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI error messages for user-correctable errors MUST provide actionable remediation instructions—including the exact command, flag, or environment variable required to resolve the defect—rather than printing raw stack traces.

Rationale: Aligning with modern compiler diagnostics (e.g. rustc) and clig.dev §Errors, actionable remediations resolve user defects instantly, whereas raw stack traces conceal solutions and elevate developer debugging friction.

**Remediation:** Catch known domain and configuration exceptions, presenting a concise description of the failure alongside a copy-pasteable remediation command or configuration example.
