---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI commands that report domain resources or configuration entities in structured format (`--json`) MUST emit the fully resolved schema with all computed and default attributes populated, rather than sparse input snippets or unparsed files.

Exception: Structured entity output MAY emit sparse or unparsed file representations IFF an explicit opt-out flag (such as `--raw`) is requested.

Rationale: In accordance with standard API and resource projection conventions (e.g. Kubernetes, OpenAPI), downstream automation requires complete, predictable schemas without having to reconstruct defaulted or derived fields.

**Remediation:** Pipe entity models through schema normalizers to hydrate default values and compute derived metadata before passing them to the JSON serialization formatter.
