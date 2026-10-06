---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI query and filtering commands MUST terminate with exit code `0` and emit an empty collection when zero matching items are found, reserving non-zero exit codes strictly for invalid syntax or execution failures.

Rationale: In line with standard query conventions (e.g. find, SQL), empty search results represent valid operational evaluations; exiting non-zero breaks downstream scripting pipelines executing under `set -e`.

**Remediation:** Return an empty JSON array `[]` or 0-byte stream with exit code `0` when filters yield no matches, directing informational notices to `stderr` only if interactive.
