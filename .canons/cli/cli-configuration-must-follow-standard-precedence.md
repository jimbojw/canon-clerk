---
inspect:
  - diff
tags:
  - cli-ergonomics
---
CLI configuration options resolution MUST adhere to strict hierarchical precedence: explicit CLI flags override environment variables, which override configuration files, which override hardcoded defaults.

Rationale: Grounded in The Twelve-Factor App and standard CLI configuration hierarchy (e.g. Viper, Cosmiconfig), this order ensures CLI flags reliably override persistent config files and environment variables during ad-hoc debugging.

**Remediation:** Implement option parsing so values supplied via command-line arguments take precedence over environment variables, falling back to loaded configuration files and finally default settings.
