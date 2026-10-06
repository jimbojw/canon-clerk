---
inspect:
  - diff
tags:
  - internal
  - architecture
  - schema
---
Code in `@canon-clerk/schema` MUST consist strictly of static TypeScript type definitions, AST interfaces, and pure validation schemas; it MUST NOT import Node.js I/O modules (`node:fs`, `node:child_process`, `node:net`, etc.) or perform side-effectful operations.

Rationale: Grounded in RFC 8927 and Clean Architecture entity boundaries, schema definitions serve as universal contracts consumed across Node, browsers, and edge runtimes; coupling them to runtime I/O violates ISO/IEC 25010 portability.

**Remediation:** Move any filesystem scanning, network requests, or environment access into `@canon-clerk/core`, keeping `@canon-clerk/schema` strictly focused on static types, Zod/JSON schemas, and AST structures.
