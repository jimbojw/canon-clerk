---
inspect:
  - diff
tags:
  - cli-ergonomics
---
Every single-character short flag supported by a CLI command MUST serve as an alias for a self-descriptive long-form flag; standalone short flags lacking long equivalents are forbidden.

Rationale: Codified in POSIX Utility Syntax Guideline 3 and GNU getopt_long standards, long-form flags ensure automated scripts and CI workflows remain self-documenting, while short aliases preserve human typing speed.

**Remediation:** Define the long-form flag (e.g. `--output`) as the primary option in the CLI definition, attaching the single-character flag (e.g. `-o`) purely as an alias.
