# Tasks

## 1. Domain Models & Core Types
- [x] 1.1 Implement `FileArtifact`, `ChangeType`, and path normalization models with unit tests
- [x] 1.2 Implement `CanonAst`, `CanonFrontmatter`, and `CanonVerdict` models with unit tests
- [x] 1.3 Implement `Caseload` aggregate state container with unit tests

## 2. Branch A Pipeline (Filing)
- [x] 2.1 Implement `intake` module for parsing unified diffs and file paths with unit tests
- [x] 2.2 Implement `discover` module for evaluating `exists:` patterns and trigger globs with unit tests
- [x] 2.3 Implement `validate` module for YAML frontmatter and Markdown AST validation with unit tests

## 3. Branch B Pipeline (Environment & Probe)
- [x] 3.1 Implement `configure` module for offline provider credential and model settings resolution with unit tests
- [x] 3.2 Implement `probe` module and `MockProviderClient` for offline provider ping with unit tests

## 4. CLI Entrypoint & Benchmarks
- [x] 4.1 Implement `clap`-based CLI in `src/main.rs` exposing `validate` subcommand and `--help`
- [x] 4.2 Benchmark cold start latency and binary size
- [x] 4.3 Run full test suite and validation check

