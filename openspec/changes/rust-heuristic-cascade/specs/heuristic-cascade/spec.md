# Spec Delta

## Purpose
Specifies the heuristic AI evaluation stages (docket, admit, audit) on the Caseload DAG using positive-polarity thresholds and structured decoding.

## ADDED Requirements

### Requirement: Macro Triage Docketing
The system SHALL evaluate candidate canons against PR and diff contexts and admit canons into the active docket when colorabilityScore >= 0.5.

#### Scenario: Docket colorable canons
- **WHEN** docket evaluates candidate canons against intake diffs
- **THEN** canons with colorabilityScore >= 0.5 are marked docketed.

### Requirement: Micro Triage Exhibit Admissibility
The system SHALL evaluate file artifacts against docketed canons and admit exhibits into evidence when admissibilityScore >= 0.5.

#### Scenario: Admit relevant evidence
- **WHEN** admit evaluates file artifacts for active docket cases
- **THEN** exhibits with admissibilityScore >= 0.5 are admitted into evidence.

### Requirement: Single-Trial Canon Audit Adjudication
The system SHALL adjudicate admitted exhibits against governing canon statutes and render compliance verdicts with line annotations.

#### Scenario: Adjudicate compliance
- **WHEN** audit evaluates admitted exhibits against canon statutes
- **THEN** audit renders a compliance verdict with status pass (>= 0.5) or fail (< 0.5).
