# Design

## Context
Canon Clerk's Caseload DAG models rule evaluation as a high-throughput court clerkship. The `apprise` node provides prospective advisory notice (*Jurisdictio Notificatoria*) before implementation begins. It evaluates design specifications, proposals, and RFC text against candidate canons, bypassing the contentious dispute track (`docket`, `admit`, `audit`) and WIP diff requirements.

## Goals / Non-Goals
**Goals:**
- Implement `ApprisalAssessment` and `CaseloadApprisal` data structures in Rust.
- Implement reason-first prompt engineering generating `apprisalSummary` before `apprisalScore`.
- Enforce exit code 2 on naked invocations and exit code 0 on zero-candidate short-circuits.
- Provide a `canon-clerk apprise` CLI command with human terminal and `--json` outputs.

**Non-Goals:**
- Multi-turn adversarial debate or violation code annotations (these belong to `audit`).

## Decisions
### 1. Data Modeling & Separation
- Add `src/models/apprise.rs` exporting `ApprisalAssessment` and `CaseloadApprisal`.
- Add `apprisal: Option<CaseloadApprisal>` to `Caseload`.

### 2. Prompt & Screening Architecture
- Implement `AppriseRunner::execute_apprise` in `src/pipeline/cascade/apprise.rs`.
- Build an aggregate single-turn prompt across candidate canons using `gemini-3.8-flash`.
- Mock provider handles `apprise` by mirroring candidate canons found in the prompt.

### 3. CLI Input Resolution
- Detect naked invocations when `--intent` is None, `paths` is empty, and stdin is a TTY.
- Read stdin if `--intent -` or if piped stdin is detected.

## Risks / Trade-offs
- **Model hallucination on unrecognized canon paths:** Ingestion maps responses strictly to known candidate canon paths.
