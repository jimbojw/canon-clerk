---
inspect:
  - diff
tags:
  - cli-ergonomics
---
Boolean CLI flags MUST be declared with positive polarity (e.g. `--color`, `--cache`, `--validate`), providing a `--no-<flag>` prefix for negation rather than introducing inherently negative flag names.

Rationale: Emphasized in clig.dev §Arguments and flags and GNU conventions, inherently negative flags create confusing double-negatives when passed boolean arguments in scripts or configuration files (e.g. `--no-validation=false`).

**Remediation:** Rename negative flags to their positive capability name (e.g. change `--skip-checks` to `--checks`), and configure the CLI framework to generate automatic `--no-<flag>` inversion flags.
