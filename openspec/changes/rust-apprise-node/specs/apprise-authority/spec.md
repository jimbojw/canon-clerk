# Spec Delta

## Purpose
Specifies the Statutory Apprisal Authority capability that evaluates design intent and prospective target paths against candidate canons to render non-contentious jurisdictional notices.

## ADDED Requirements

### Requirement: Missing Input Source Guard
The system SHALL fail fast with exit code 2 and usage guidance when invoked without design intent, piped standard input, or target paths.

#### Scenario: Naked invocation fails fast
- **WHEN** apprise is invoked without intent text, standard input, or target paths
- **THEN** execution terminates with exit code 2 and actionable usage hints.

### Requirement: Zero Candidate Canon Short Circuit
The system SHALL terminate immediately with exit code 0 and empty assessments without dispatching LLM calls when prospective target paths trigger zero candidate canons.

#### Scenario: Short circuit on empty candidate set
- **WHEN** prospective target paths trigger zero candidate canons
- **THEN** execution terminates with exit code 0 and zero model calls are dispatched.

### Requirement: Prospective Statutory Apprisal Screening
The system SHALL evaluate candidate canons against design intent using reason-first structured decoding and assign status applicable for scores greater than or equal to threshold.

#### Scenario: Screen candidate canons against intent
- **WHEN** apprise evaluates candidate canons against design intent
- **THEN** canons with apprisalScore >= threshold receive status applicable and others receive dismissed.
