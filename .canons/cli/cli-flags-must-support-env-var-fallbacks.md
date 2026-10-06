---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI flags configuring persistent operational options (such as API keys, model selectors, and log verbosity) MUST support configuration via environment variables when the command-line flag is omitted.

Rationale: Codified in The Twelve-Factor App §III (Config) and clig.dev §Environment variables, environment variable fallbacks prevent credential leaks in process tables and support seamless container and secret-manager injection.

**Remediation:** Register environment variable fallbacks (e.g. `GEMINI_API_KEY` for `--gemini-api-key`) in the CLI option definition, ensuring flag values take precedence when explicitly supplied.
