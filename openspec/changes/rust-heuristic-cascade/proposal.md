# Proposal

## Why
With Branch A (Filing) and Branch B (Environment/Probe) fully operational and verified against live Google Gemini endpoints, Canon Clerk requires the AI-driven **Heuristic Cascade** to complete its evaluation lifecycle:
- Macro Triage (`docket`): Subject-matter jurisdiction screening (`colorabilityScore >= 0.5`).
- Micro Triage (`admit`): Relevance screening of file artifacts into evidence (`admissibilityScore >= 0.5`).
- Single-Trial Adjudication (`audit`): Substantive compliance trial of admitted exhibits against governing canon statutes (`complianceScore >= 0.5`).

## What Changes
- Implements cascade data representations in `src/models/cascade.rs`: `ColorabilityAssessment`, `AdmittedExhibit`, `CanonAdjudication`, and `CodeAnnotation`.
- Implements prompt builders and structured JSON schemas conforming to Gemini response requirements.
- Implements `execute_docket`, `execute_admit`, and `execute_audit` pipeline stages.
- Integrates structured AI execution into `GoogleProviderClient` and `MockProviderClient`.
- Exposes CLI subcommands `docket`, `admit`, and `audit`.

## Capabilities
### New Capabilities
- `heuristic-cascade`: Autonomous AI evaluation cascade across docket, admit, and audit stages.

### Modified Capabilities
None.

## Impact
- Enables end-to-end Caseload DAG execution from in-flight diff intake through binding compliance verdicts.
- Supports both offline testing via mock providers and live adjudication via Google Gemini.
