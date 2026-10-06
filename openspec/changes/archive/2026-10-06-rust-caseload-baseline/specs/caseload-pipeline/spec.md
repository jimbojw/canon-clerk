# Spec Delta

## Purpose
Establishes the foundational Caseload pipeline domain logic and execution engine in Rust, covering Branch A intake/discovery/validation and Branch B configuration/probe.

## ADDED Requirements

### Requirement: Caseload Domain Representation
The system SHALL represent Caseload state containing indexed file artifacts, parsed canons, configuration context, and adjudication verdicts.

#### Scenario: Caseload initialization
- **WHEN** a new caseload is instantiated with file artifacts and canons
- **THEN** the caseload maintains structured collections of artifacts and verdicts.

### Requirement: Branch A Intake Processing
The system SHALL ingest unified diffs and repository file paths into normalized FileArtifact structures.

#### Scenario: Ingest diff and path lists
- **WHEN** a patch string or list of file paths is supplied to intake
- **THEN** intake produces indexed FileArtifact entries with change statuses.

### Requirement: Branch A Discovery Evaluation
The system SHALL evaluate file existence patterns and trigger conditions against file artifacts to identify active canons.

#### Scenario: Trigger match discovery
- **WHEN** a canon specifies trigger glob patterns matching intake artifacts
- **THEN** discover includes the canon in the caseload active set.

### Requirement: Branch A Canon Validation
The system SHALL parse Markdown frontmatter and canon body ASTs, verifying rule schemas and falsifiability invariants.

#### Scenario: Validate canon AST
- **WHEN** a canon document is validated
- **THEN** validate parses YAML frontmatter and extracts Markdown AST sections or reports syntax errors.

### Requirement: Branch B Configuration Resolution
The system SHALL resolve model parameters and offline API credentials following standard hierarchy precedence.

#### Scenario: Resolve configuration
- **WHEN** configuration options are evaluated with environment variables and defaults
- **THEN** configure yields structured provider settings without network access.

### Requirement: Branch B Connectivity Probe
The system SHALL probe provider reachability using configurable HTTP or mock clients.

#### Scenario: Mock provider probe
- **WHEN** a probe is executed against the mock provider
- **THEN** probe returns a healthy ping response and diagnostic latency.

