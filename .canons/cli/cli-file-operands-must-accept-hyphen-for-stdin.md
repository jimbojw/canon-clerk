---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands that accept file operands MUST accept `-` to read from standard input (`stdin`) in place of a named filesystem path.

Rationale: Following POSIX.1-2017 Utility Syntax Guideline 10, recognizing `-` as standard input enables commands to compose cleanly into Unix pipelines without requiring intermediate temporary files.

**Remediation:** Inspect positional file operands for `-`, reading from `process.stdin` instead of attempting to open a file named `"-"` on disk.
