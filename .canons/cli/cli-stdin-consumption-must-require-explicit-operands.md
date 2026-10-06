---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands MUST NOT read standard input based on stream TTY inspection or implicit pipe detection; standard input MUST be consumed exclusively when `-` is explicitly supplied as a positional operand.

Rationale: Citing POSIX.1-2017 Utility Syntax Guideline 10 and *Command Line Interface Guidelines* (clig.dev §Pipes), sniffing `isTTY` or pipe presence to read stdin implicitly causes commands to hang indefinitely in CI runners and subshells that allocate open, inactive standard input streams without EOF. Explicit hyphen operands enforce predictable batch execution and protect interactive invocation.
