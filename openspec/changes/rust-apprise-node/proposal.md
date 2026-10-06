# Proposal

## Why
Issue #215 and the newly merged architecture specification (`docs/architecture/nodes/apprise.md`) introduce the prospective Statutory Apprisal Authority (*Jurisdictio Notificatoria*) into the Caseload DAG. This capability informs developers and autonomous coding agents of governing canons at design/planning time, prior to authoring code or generating git diffs. Implementing `apprise` in the native Rust engine provides prospective pre-implementation rule screening with zero cold-start latency.

## What Changes
1. **Domain Models**: Implement `ApprisalAssessment` and `CaseloadApprisal` in `src/models/apprise.rs` and enrich `Caseload`.
2. **Apprise Runner & Prompts**: Implement `AppriseRunner` and `CascadePrompts::build_apprise_prompt` supporting reason-first structured JSON generation (`apprisalSummary` before `apprisalScore`).
3. **Short-Circuit & Guard Invariants**: Enforce naked invocation guard (exit 2) and zero-candidate short-circuit (exit 0 without LLM dispatch).
4. **CLI Subcommand**: Add `canon-clerk apprise` subcommand supporting `--intent`, `-` stdin streaming, prospective target paths, `--threshold`, `--mock`, and `--json`.

## Capabilities
### New Capabilities
- `apprise-authority`: Prospective statutory apprisal authority screening candidate canons against design intent.

## Impact
- Extends `src/models/`, `src/pipeline/`, and `src/main.rs`.
- Zero impact on existing `validate`, `probe`, `docket`, `admit`, and `audit` subcommands.
