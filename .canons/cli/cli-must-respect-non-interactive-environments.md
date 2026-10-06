---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands MUST detect non-interactive or automated environments (`!isTTY`, `CI=true`, or `NO_COLOR=1`) and automatically disable interactive prompts, ANSI color escapes, and animated terminal spinners.

Rationale: Per the NO_COLOR standard (no-color.org) and clig.dev §Interactivity, interactive prompts hang unattended CI jobs indefinitely, while ANSI escapes and spinners pollute log aggregators with control sequence noise.

**Remediation:** Check terminal TTY status and standard environment variables (`CI`, `NO_COLOR`) before initiating prompts or ANSI rendering, terminating with an actionable non-zero exit code if required parameters are missing in non-interactive mode.
