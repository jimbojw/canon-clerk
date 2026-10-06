---
inspect:
  - diff
tags:
  - cli-ergonomics
---
Subcommands in hierarchical CLI command trees MUST configure argument parser exit overrides and error formatters to ensure that option, argument, and syntax errors uniformly terminate with exit status `2` across all subcommand nesting levels.

Rationale: Following GNU Coding Standards §4.3 and BSD `sysexits.h` conventions, command-line interfaces standardly reserve exit code `2` for syntax and argument parsing errors, distinguishing invocation defects from domain validation failures (`exit 1`). In hierarchical CLI frameworks, subcommands default to generic exit codes unless exit-handling is explicitly overridden.
