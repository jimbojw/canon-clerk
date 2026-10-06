---
inspect:
  - diff
tags:
  - internal
  - architecture
  - cli
---
The command-line interface (`@canon-clerk/cli`) MUST act strictly as a presentation and orchestration adapter that translates user arguments and flags into calls to `@canon-clerk/core`; it MUST NOT implement canon discovery, cascade resolution, or path matching algorithms directly.

Rationale: Grounded in Ports and Adapters and Command Line Interface Guidelines (clig.dev), the CLI is a driving presentation adapter; duplicating domain queries or cascade logic outside `@canon-clerk/core` causes cross-layer drift and untestable terminal coupling.

**Remediation:** Author CLI command handlers to parse terminal flags, invoke appropriate domain queries or evaluation services in `@canon-clerk/core`, and format the resulting data for terminal display.
