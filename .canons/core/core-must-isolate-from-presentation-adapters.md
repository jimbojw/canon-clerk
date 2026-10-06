---
tags:
  - internal
  - architecture
  - core
---
Code in `@canon-clerk/core` MUST NOT import presentation adapters, including `@canon-clerk/cli`, `@canon-clerk/action`, or runner libraries (`@actions/*`).

Rationale: Citing Clean Architecture's Dependency Rule, core domain policies must remain independent of delivery mechanisms to prevent architectural inversion and circular coupling.
