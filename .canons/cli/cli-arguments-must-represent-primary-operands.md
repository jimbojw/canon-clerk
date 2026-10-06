---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands MUST accept their primary operational targets (such as file paths, canon directories, or commit references) as positional arguments rather than requiring named flags.

Rationale: Formulated in POSIX Utility Syntax Guidelines 4 & 5 and clig.dev ("Arguments for what, flags for how"), positional operands optimize shell expansion and eliminate redundant flag boilerplate.

**Remediation:** Redesign the command signature so primary subjects are declared as positional arguments (e.g. `canon-clerk check <path>` instead of `canon-clerk check --path <path>`), reserving named flags for optional modifiers.
