---
inspect:
  - diff
tags:
  - internal
  - architecture
  - core
---
The core engine (`@canon-clerk/core`) MUST NOT format terminal UI (colors, tables), emit unstructured logs directly to `stdout`/`stderr`, or invoke `process.exit()`. It MUST return structured data payloads and signal execution failures via errors or return types.

Rationale: Grounded in the Unix Rule of Separation ("Separate engine from interface") and Hexagonal Architecture, core evaluation must run headlessly across CLIs, GitHub Actions, and programmatic SDKs; hardcoded console formatting or process termination breaks non-terminal adapters.

**Remediation:** Return structured data objects (such as `FilterResult` or `AuditResult`) from core methods, and delegate terminal tables, ANSI coloring, and exit code handling to `packages/cli`.
