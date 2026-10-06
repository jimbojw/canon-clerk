# Design

## Context
Canon Clerk's core engine models changes as an active court clerkship:
- `docket`: Colorability (Subject-matter jurisdiction over change)
- `admit`: Admissibility (Relevance of file diffs to active cases)
- `audit`: Compliance (Single-trial adjudication of evidence vs statute)

Each stage acts as a sieve to eliminate non-applicable canons and non-relevant files before expensive deep reasoning is invoked.

## Goals / Non-Goals
**Goals:**
- Implement all three cascade stages in Rust with positive polarity (`score >= 0.5`).
- Structured JSON prompt engineering with Gemini API schema enforcement.
- Pure aggregate state container enrichment on `Caseload`.
- CLI subcommands `canon-clerk docket`, `canon-clerk admit`, and `canon-clerk audit`.

**Non-Goals:**
- Multi-agent debate or adversarial critique loops in this phase.

## Decisions
### 1. Unified Cascade Module Structure
- Place domain types in `src/models/cascade.rs`.
- Implement cascade stage runners under `src/pipeline/cascade/`:
  - `docket.rs`
  - `admit.rs`
  - `audit.rs`
  - `prompts.rs`

### 2. Structured JSON Generation via Google Gemini
- Use Gemini `responseMimeType: "application/json"` with schema instructions embedded in prompt and request options.
- Parse responses into strongly-typed Rust Serde structs.

## Risks / Trade-offs
- **Model hallucination on unknown canon keys:** The prompt strictly enumerates candidate canon IDs and checks for unrecognized keys in validation.
