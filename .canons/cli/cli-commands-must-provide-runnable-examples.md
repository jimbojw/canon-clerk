---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI command help screens (`--help`) MUST include at least one realistic, copy-pasteable runnable usage example demonstrating common flag configurations alongside argument definitions.

Rationale: Following GNU Coding Standards §4.8 and clig.dev §Help, concrete examples communicate workflow invocation context and argument combinations that isolated, abstract flag lists cannot convey.

**Remediation:** Include an `Examples:` block within the command help metadata displaying representative, functional command invocations for primary use cases.
