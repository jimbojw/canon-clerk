---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands that report inspection results, audit verdicts, status summaries, or diagnostic lists MUST support a `--json` flag that outputs valid, unadorned JSON to `stdout`.

Rationale: In accordance with modern CLI standards (clig.dev §Output), automated CI systems and AI agents require deterministic machine-readable JSON payloads rather than fragile scraping of human-oriented terminal text.

**Remediation:** Implement a `--json` flag that serializes command results directly via `JSON.stringify()` to `stdout`, ensuring all accompanying decorative formatting and status banners are omitted.
