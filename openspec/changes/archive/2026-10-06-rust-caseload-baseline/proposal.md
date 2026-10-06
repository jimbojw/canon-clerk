# Proposal

## Why
Canon Clerk enforces engineering canons across repository changes using a directed acyclic graph (Caseload DAG). To achieve strict performance goals (<5ms offline CLI pre-flight, zero-token Branch A evaluation) and lightweight single-binary distribution, this spike evaluates the feasibility, developer ergonomics, and runtime footprint of re-implementing the core Caseload pipeline in Rust under a strict zero-human-authored-code frame.

## What Changes
- Implements core domain models in Rust: `Caseload`, `FileArtifact`, `CanonAst`, `CanonVerdict`.
- Implements the Branch A (Filing) pipeline:
  - `intake`: Parse unified diffs and ingest file paths into normalized artifacts.
  - `discover`: Match active canons against file artifacts via `exists:` and trigger glob rules.
  - `validate`: Parse Markdown frontmatter and AST structures; enforce rule schema validity.
- Implements the Branch B (Environment) pipeline:
  - `configure`: Resolve model configuration and offline credentials following hierarchy precedence.
  - `probe`: Provide diagnostic connectivity probes with an offline mock provider client.
- Implements a high-performance CLI entrypoint with `clap` and benchmark cold start pre-flight latency.

## Capabilities
### New Capabilities
- `caseload-pipeline`: End-to-end Caseload DAG domain execution across Branch A (Filing) and Branch B (Environment).

### Modified Capabilities
None.

## Impact
- Replaces TypeScript/Node.js monorepo runtime with a compiled, single-binary Rust implementation.
- Preserves the repository's OpenSpec spec-driven cycle.
- Eliminates Node.js runtime startup overhead for CLI pre-flight.

