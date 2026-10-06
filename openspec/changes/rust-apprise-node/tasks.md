# Tasks

## 1. Apprise Data Models & Prompt Engineering
- [ ] 1.1 Implement `ApprisalAssessment` and `CaseloadApprisal` in `src/models/apprise.rs` with unit tests
- [ ] 1.2 Implement `CascadePrompts::build_apprise_prompt` and system instruction with unit tests

## 2. Apprise Pipeline Stage
- [ ] 2.1 Implement `AppriseRunner::execute_apprise` in `src/pipeline/cascade/apprise.rs` with unit tests
- [ ] 2.2 Wire `MockProviderClient` structured JSON generation for `apprise`

## 3. CLI Integration & Verification
- [ ] 3.1 Implement `AppriseArgs` and `run_apprise` in `src/main.rs` with missing input guard (exit 2) and short-circuit (exit 0)
- [ ] 3.2 Verify `apprise` subcommand with `--mock` and live Gemini provider
