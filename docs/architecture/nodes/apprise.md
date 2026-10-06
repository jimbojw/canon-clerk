# Statutory Apprisal (`apprise`)

**Status:** Authoritative Architectural Standard  
**Core Domain Engine:** `@canon-clerk/core`  
**Driving Adapters:** `@canon-clerk/cli` (`apprise`), `@canon-clerk/action`, `@canon-clerk/integration-tests-private`

---

## 1. Domain Concept & Role (`core`)

`apprise` performs **Prospective Jurisprudential Screening: Statutory Apprisal for Design-Time Guidance** across all candidate canons matching the prospective target scope.

In the court clerkship taxonomy, `apprise` represents the court exercising its **Apprisal Authority** (*Jurisdictio Notificatoria*). Rather than functioning as a trial judge hearing evidence on past infractions (the contentious `audit` path), `apprise` acts as the **Clerk of the Court apprising parties of governing rules**. It receives a prospective design inquiry—formulated as planned file targets and design intent—and determines which canons claim subject-matter jurisdiction over the planned work, providing contributors and AI coding agents with pre-flight notice of governing statutes before implementation begins.

- **Imperative Verb:** `apprise`
- **Court Clerkship Role:** Statutory apprisal, procedural notice, and jurisdictional applicability determination.
- **Metric Pair:** `apprisalScore` (number [0.0, 1.0]) and `apprisalSummary` (string rationale justifying prospective jurisdiction).
- **Core Question:** *"Given this prospective design intent and target scope, does this canon have a colorable claim of jurisdiction over the planned work?"*

---

## 2. Dependencies & Prerequisites (`core`)

- **Direct Prerequisites:**
  - `validate` (Branch A: validated candidate canons in `caseload.discovery.candidateCanons` and `caseload.validation`).
  - `configure` (Branch B: resolved provider credentials and model specifiers in `caseload.config`).
- **Transitive Prerequisites:** `intake`, `discover`.
- **Branch Independence & Pruning:**
  - Evaluates without requiring code diffs, patches, or work-in-progress (WIP) exhibits.
  - Decoupled from the contentious dispute track: completely prunes `docket`, `admit`, and `audit` from execution.
  - Diagnostic leaf `probe` is never scheduled.

---

## 3. Core Functional Contract (`packages/core`)

```ts
export interface AppriseOptions {
  /** Minimum apprisal salience threshold [0.0, 1.0] to designate status as 'applicable' (default: 0.5) */
  readonly threshold?: number | undefined;

  /** Optional screener model override (defaults to caseload.config.screenerModel or 'google:gemini-3.5-flash-lite') */
  readonly screenerModel?: string | undefined;

  /** Injectable ModelClient for unit testing or custom provider overrides */
  readonly client?: ModelClient | undefined;

  /** Cancellation and timeout signal */
  readonly signal?: AbortSignal | undefined;
}

export function executeApprise(
  options: AppriseOptions,
  caseload: Caseload
): Promise<Caseload>;
```

### Caseload Delta
Populates the `.apprisal` field on the cumulative `Caseload`:

```ts
export interface ApprisalAssessment {
  /** Relevance score indicating prospective jurisdiction over the stated intent [0.0, 1.0] */
  readonly apprisalScore: number;

  /** Rationale explaining why this canon governs (or does not govern) the prospective intent */
  readonly apprisalSummary: string;

  /** Status outcome */
  readonly status: 'applicable' | 'dismissed';
}

export interface CaseloadApprisal {
  /** Apprisal assessments keyed by canon path */
  readonly assessments: Record<string, ApprisalAssessment>;
}
```

### Domain Short-Circuit Invariant
If `caseload.discovery.candidateCanons.length === 0`:
- Execution terminates immediately with exit code `0`.
- An empty apprisal container (`assessments: {}`) is attached to `caseload.apprisal`.
- Zero AI model calls are dispatched, preserving tokens and execution latency.

---

## 4. Process & Domain Logic (`core`)

1. **Candidate Canon Ingestion & Early Exit:**  
   Extracts `candidateCanons` from `caseload.discovery` (resolved from prospective target paths or globs) along with `caseload.intake.intent` (and any piped specification or RFC text). If `candidateCanons.length === 0`, short-circuits immediately with exit code `0`.
2. **Aggregate Single-Turn Screening (`gemini-3.5-flash-lite`):**  
   Evaluates **all candidate canons against the prospective intent in a single aggregate prompt turn** using `caseload.config.screenerModel`.
3. **Constrained Decoding Schema (Reason-First):**  
   Enforces structured JSON output generating `apprisalSummary` before `apprisalScore`:
   ```json
   {
     "assessments": {
       "packages/auth/.canons/cacheing-layers-must-have-configurable-expiry.md": {
         "apprisalSummary": "Proposed round-robin dispatch introduces a dynamically updated provider cache, which must define explicit expiration policies.",
         "apprisalScore": 0.92,
         "status": "applicable"
       },
       "packages/auth/.canons/database-migrations-must-include-rollback-instructions.md": {
         "apprisalSummary": "The planned refactor only modifies in-memory provider dispatch and does not alter database schemas or migrations.",
         "apprisalScore": 0.05,
         "status": "dismissed"
       }
     }
   }
   ```
   Generating `apprisalSummary` before `apprisalScore` provides a chain-of-thought scratchpad, anchoring reproducible probability distributions.
4. **Threshold Gate (`apprisalScore >= threshold`, default: `0.5`):**  
   - Canons scoring $\ge 0.5$ establish prospective jurisdiction and are marked `status: 'applicable'`.
   - Canons scoring $< 0.5$ are marked `status: 'dismissed'` with summary rationale.
5. **Latency & Token Economy:** Executes in ~400ms, expending only ~1,200 tokens across 20+ candidate canons.

---

## 5. Driving Adapter: CLI (`packages/cli`)

The CLI exposes `apprise` as the primary entry point for design-time and pre-flight planning:

```bash
# 1. Inline design intent with prospective target paths:
canon-clerk apprise --intent "Refactor authentication to round-robin between providers" "packages/auth/src" "packages/app/src/frontend"

# 2. Piping an OpenSpec proposal or design RFC from standard input:
cat openspec/changes/provider-rotation/proposal.md | canon-clerk apprise packages/auth/src --intent -

# 3. Running against an upstream Caseload file:
canon-clerk apprise --caseload upstream-discovery.json

# 4. Machine-readable JSON output for AI Agent prompt injection:
canon-clerk apprise --intent "Add Kafka event bus" packages/events/src --json
```

### Missing Input Source Guard (Naked Invocation)
Invoking `canon-clerk apprise` naked without an intent, target paths, or upstream caseload fails fast with exit code `2` (Usage Error) and prints actionable guidance:
```text
error: No design intent or target paths provided for apprise.
  Hint: Provide an intent ('--intent <text>'), pipe a specification via standard input ('-'),
        specify prospective target paths, or pass an upstream caseload ('--caseload <path>').
```

Conversely, if prospective target paths yield zero candidate canons in `discover`, the command short-circuits cleanly with exit code `0` ("0 canons triggered by prospective target scope; no applicable constraints").

### CLI Output Modes
- **Default (Terminal / Stylish):** Renders a structured Markdown apprisal report on `stdout` listing applicable canons alongside their `apprisalSummary` rationales.
- **`--json`:** Emits the cumulative `Caseload` JSON containing the fully populated `.apprisal` container.

### CLI Exit Codes
- **0:** Successful statutory apprisal (including clean short-circuits with 0 applicable canons).
- **2:** Usage error, missing input source (naked invocation), provider connection failure, or invalid arguments.

---

## 6. Driving Adapter: GitHub Action (`packages/action`)

In CI and pull request automation, `apprise` is utilized in **Pre-Implementation & Draft PR Workflows**:
1. **Draft PR Guidance:** When an author opens a Draft PR or an issue with an architectural specification, the action runs `executeApprise` against the PR description and modified paths.
2. **Apprisal Comment / Step Summary:** Posts a non-blocking `GITHUB_STEP_SUMMARY` or pull request comment summarizing applicable canons and their applicability rationales, providing notice *before* substantive review.
3. **Zero-Failure Gate:** As a procedural notice, `apprise` never fails a CI build (`conclusion: 'neutral'` or `'success'`).

---

## 7. Driving Adapter: Integration Tests (`packages/integration-tests-private`)

Integration tests invoke `executeApprise` against simulated design intents, verifying that:
- Prospective intent queries accurately identify applicable canons and reject inapplicable rules.
- `apprisalSummary` rationales and `apprisalScore` distributions are grounded and reproducible.
- Machine-readable JSON schemas strictly validate across runs without missing fields.

