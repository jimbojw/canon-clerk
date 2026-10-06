# Tasks

## 1. Cascade Data Models & Prompts
- [x] 1.1 Implement `ColorabilityAssessment`, `AdmittedExhibit`, `CanonAdjudication`, and `CodeAnnotation` in `src/models/cascade.rs` with unit tests
- [x] 1.2 Implement prompt builders and structured JSON generation in `src/pipeline/cascade/prompts.rs` with unit tests

## 2. Docket and Admit Stages
- [x] 2.1 Implement `execute_docket` in `src/pipeline/cascade/docket.rs` with unit tests and mock provider
- [x] 2.2 Implement `execute_admit` in `src/pipeline/cascade/admit.rs` with unit tests and mock provider

## 3. Audit Stage & CLI Integration
- [x] 3.1 Implement `execute_audit` in `src/pipeline/cascade/audit.rs` with unit tests and mock provider
- [x] 3.2 Wire `docket`, `admit`, and `audit` subcommands into CLI in `src/main.rs`
- [x] 3.3 Verify with live Gemini provider and run full test suite
