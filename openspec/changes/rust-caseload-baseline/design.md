# Design

## Context
Canon Clerk enforces invariant canons on repository changes. The Caseload DAG evaluates Branch A (Filing: intake, discover, validate) and Branch B (Environment: configure, probe) independently before converging into docketing and adjudication.
Migrating from TypeScript to Rust promises sub-millisecond CLI pre-flight, zero-token Branch A filtering, and a statically typed pipeline without Node.js startup penalty. This design implements the core domain models and pipeline stages in idiomatic Rust under an autonomous zero-human-authored-code framework.

## Goals / Non-Goals
**Goals:**
- Zero-human-authored code: 100% authored by AI agents via OpenSpec.
- Type-safe domain models for `Caseload`, `FileArtifact`, `CanonAst`, `CanonVerdict`.
- Deterministic, offline Branch A pipeline (`intake`, `discover`, `validate`).
- Flexible Branch B pipeline (`configure`, `probe`) supporting offline mock execution.
- Fast CLI binary with sub-5ms pre-flight execution and comprehensive unit tests.

**Non-Goals:**
- External live LLM network requests in this spike; provider connectivity is demonstrated through mock and offline interfaces.
- Replacing the OpenSpec harness itself (retains `@fission-ai/openspec` in root `package.json`).

## Decisions
### 1. Unified Single-Crate Architecture
- **Decision:** Place both the core library (`src/lib.rs`) and CLI entrypoint (`src/main.rs`) within a single Cargo package.
- **Rationale:** Minimizes workspace build configuration complexity during the spike while preserving clear separation between library modules and binary CLI commands.
- **Alternatives Considered:** Cargo workspace with separate crates (`crates/core`, `crates/cli`). Rejected for initial spike to maximize compilation speed and reduce configuration surface.

### 2. CommonMark AST & Frontmatter Parsing
- **Decision:** Split YAML frontmatter delimited by `---` and parse frontmatter with `serde_yaml`. Parse Markdown body into AST nodes via `pulldown-cmark`.
- **Rationale:** `pulldown-cmark` provides pull-based zero-copy event streaming conforming to CommonMark standards.

### 3. Path Matching via Standard Glob
- **Decision:** Implement trigger and `exists:` rule pattern evaluation using the `glob` crate.
- **Rationale:** Standardized POSIX and glob matching semantics matching Canon Clerk's file matching requirements.

### 4. Provider Abstraction via Traits
- **Decision:** Define a `ProviderClient` trait with asynchronous `probe()` method, accompanied by a `MockProviderClient` for deterministic offline verification.
- **Rationale:** Allows offline testing without external network calls while leaving room for live `reqwest`-based Gemini/Anthropic providers.

## Risks / Trade-offs
- **Serde YAML status:** The `serde_yaml` crate is deprecated upstream, but remains the standard YAML parser in Rust. For this baseline spike it is sufficient and well-tested.
- **Diff parsing edge cases:** Parsing arbitrary unified diffs can be tricky. A dedicated parser in `intake` handles standard `diff --git`, `---`, `+++`, and `@@` chunk headers.
