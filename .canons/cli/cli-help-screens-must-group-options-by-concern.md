---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands that expose more than six configuration options MUST organize their `--help` screen flags into categorized sections (such as Execution, Output, and Credentials) rather than displaying a flat list.

Rationale: Following GNU Coding Standards §4.8 and modern CLI help conventions (e.g. cargo, kubectl), option grouping provides progressive disclosure, preventing advanced flags from obscuring primary workflow options.

**Remediation:** Configure the CLI argument parser with option groups or categories (e.g. `Input Options`, `Execution Options`, `Output Options`) to provide structured progressive disclosure in `--help` output.
