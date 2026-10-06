---
inspect:
  - diff
tags:
  - cli-ergonomics
---
When emitting plain text to non-interactive streams or pipelines, CLI list and query commands MUST default to unadorned, newline-separated records (one item per line).

Exception: A non-interactive stream MAY emit alternate representations (such as structured JSON or rendered tables) IFF an explicit format flag (such as `--json` or `--table`) is requested.

Rationale: In accordance with POSIX stream processing and clig.dev §Output, unadorned single-line records ensure list outputs compose directly with standard Unix utilities (`xargs`, `wc`, `grep`) without requiring custom regex post-processing.

**Remediation:** Wrap formatting decorators (column headers, borders, and ANSI styles) behind TTY checks or explicit layout flags, streaming raw delimiter-separated records when piped to `stdout`.
