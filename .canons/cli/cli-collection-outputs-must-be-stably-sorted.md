---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands that report lists or collections of items (such as canons, diagnostic findings, or files) MUST output them in a deterministic, stable sort order.

Rationale: Following the Reproducible Builds standard (reproducible-builds.org) and POSIX collation rules, deterministic sorting eliminates spurious snapshot diffs, flaky test assertions, and cognitive confusion in automated diff evaluation.

**Remediation:** Apply an explicit natural alphanumeric sort (e.g. sorting by file path, line number, or canonical identifier) to result collections prior to terminal rendering or JSON serialization.
