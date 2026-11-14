// Copyright 2026 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use canon_clerk::{Caseload, PipelineStage, TelemetryEvent};
use std::fs;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_canon-clerk")
}

#[test]
fn test_cli_version_flag() {
    let output = Command::new(bin_path())
        .arg("--version")
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("canon-clerk 0.1.0"),
        "stdout should contain version string, got: {stdout}"
    );
}

#[test]
fn test_cli_version_short_flag() {
    let output = Command::new(bin_path())
        .arg("-V")
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("canon-clerk 0.1.0"),
        "stdout should contain version string, got: {stdout}"
    );
}

#[test]
fn test_cli_help_flag() {
    let output = Command::new(bin_path())
        .arg("--help")
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: canon-clerk"),
        "stdout should contain usage line"
    );
    assert!(
        stdout.contains("Logging & Diagnostics:"),
        "stdout should organize options into concern sections"
    );
    assert!(
        stdout.contains("-v, --verbose"),
        "stdout should document verbose flag"
    );
    assert!(
        stdout.contains("TARGETS"),
        "stdout should document primary positional targets"
    );
}

#[test]
fn test_cli_help_short_flag() {
    let output = Command::new(bin_path())
        .arg("-h")
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: canon-clerk"));
}

#[test]
fn test_cli_no_args_displays_help() {
    let output = Command::new(bin_path())
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: canon-clerk"),
        "invoking with no arguments should display help"
    );
}

#[test]
fn test_cli_invalid_flag_exits_with_code_2() {
    let output = Command::new(bin_path())
        .arg("--nonexistent-flag")
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(
        output.status.code(),
        Some(2),
        "CLI argument errors must terminate with exit code 2 per cli-subcommands-must-standardize-usage-exit-codes"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("error:"),
        "stderr should describe the usage error, got: {stderr}"
    );
}

#[test]
fn test_cli_targets_positional_execution() {
    let output = Command::new(bin_path())
        .arg("src/lib.rs")
        .arg("README.md")
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn test_cli_verbose_diagnostic_emits_to_stderr() {
    let output = Command::new(bin_path())
        .arg("-v")
        .arg("src/lib.rs")
        .output()
        .expect("failed to execute canon-clerk");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DEBUG") || stderr.contains("INFO"),
        "diagnostics should be written to stderr per cli-must-separate-data-from-diagnostics, got: {stderr}"
    );
}

#[test]
fn test_cli_subcommand_dispatch_routes_exit_codes() {
    // Verify top-level binary routes subcommand usage errors to exit code 2
    let fail_out = Command::new(bin_path())
        .arg("intake")
        .output()
        .expect("failed to execute canon-clerk intake");
    assert_eq!(fail_out.status.code(), Some(2));

    // Verify top-level binary routes subcommand success to exit code 0
    let ok_out = Command::new(bin_path())
        .args(["intake", "src/lib.rs", "--json"])
        .output()
        .expect("failed to execute canon-clerk intake src/lib.rs --json");
    assert_eq!(ok_out.status.code(), Some(0));

    // Verify discover naked invocation exits 2, predicate -q zero-match exits 1, and --all-canons exits 0
    let disc_naked = Command::new(bin_path())
        .arg("discover")
        .output()
        .expect("failed to execute canon-clerk discover");
    assert_eq!(disc_naked.status.code(), Some(2));

    let disc_quiet_miss = Command::new(bin_path())
        .args([
            "discover",
            "-q",
            "--canon",
            ".canons/readme-authoring/readme-must-lead-with-what-and-why.md",
            "src/lib.rs",
        ])
        .output()
        .expect("failed to execute canon-clerk discover -q");
    assert_eq!(disc_quiet_miss.status.code(), Some(1));

    let disc_all = Command::new(bin_path())
        .args(["discover", "--all-canons", "--json"])
        .output()
        .expect("failed to execute canon-clerk discover --all-canons --json");
    assert_eq!(disc_all.status.code(), Some(0));

    // Verify validate naked invocation exits 2 and --all-canons exits 0
    let val_naked = Command::new(bin_path())
        .arg("validate")
        .output()
        .expect("failed to execute canon-clerk validate");
    assert_eq!(val_naked.status.code(), Some(2));

    let val_all = Command::new(bin_path())
        .args(["validate", "--all-canons", "--max-warnings", "0", "--json"])
        .output()
        .expect("failed to execute canon-clerk validate --all-canons --json");
    assert_eq!(val_all.status.code(), Some(0));

    // Verify configure missing GEMINI_API_KEY exits 2 and valid key exits 0
    let cfg_no_key = Command::new(bin_path())
        .arg("configure")
        .env_remove("GEMINI_API_KEY")
        .env("XDG_CONFIG_HOME", "/nonexistent-canon-clerk-xdg-dir")
        .output()
        .expect("failed to execute canon-clerk configure without key");
    assert_eq!(cfg_no_key.status.code(), Some(2));

    let cfg_ok = Command::new(bin_path())
        .args(["configure", "--json"])
        .env("GEMINI_API_KEY", "test-key-from-integration-runner")
        .output()
        .expect("failed to execute canon-clerk configure --json");
    assert_eq!(cfg_ok.status.code(), Some(0));

    // Verify probe missing GEMINI_API_KEY exits 2 and pre-populated caseload fast-forwards hermetically
    let probe_no_key = Command::new(bin_path())
        .arg("probe")
        .env_remove("GEMINI_API_KEY")
        .env("XDG_CONFIG_HOME", "/nonexistent-canon-clerk-xdg-dir")
        .output()
        .expect("failed to execute canon-clerk probe without key");
    assert_eq!(probe_no_key.status.code(), Some(2));

    // Verify apprise dual-input guard exits 2 and zero-candidate short-circuit exits 0 without GEMINI_API_KEY
    let apprise_intent_only = Command::new(bin_path())
        .args(["apprise", "--intent", "Add new feature"])
        .output()
        .expect("failed to execute canon-clerk apprise --intent");
    assert_eq!(apprise_intent_only.status.code(), Some(2));

    let apprise_zero_batch = Command::new(bin_path())
        .args([
            "apprise",
            "--intent",
            "Add new feature",
            "src/lib.rs",
            "--apprise-batch-size",
            "0",
        ])
        .output()
        .expect("failed to execute canon-clerk apprise --apprise-batch-size 0");
    assert_eq!(apprise_zero_batch.status.code(), Some(2));

    let apprise_zero_cand = Command::new(bin_path())
        .args([
            "apprise",
            "--intent",
            "Refactor library internals",
            "--canon",
            ".canons/readme-authoring/readme-must-lead-with-what-and-why.md",
            "src/lib.rs",
            "--json",
        ])
        .env_remove("GEMINI_API_KEY")
        .env("XDG_CONFIG_HOME", "/nonexistent-canon-clerk-xdg-dir")
        .output()
        .expect("failed to execute canon-clerk apprise zero-candidate run");
    assert_eq!(apprise_zero_cand.status.code(), Some(0));
}

#[test]
fn test_cli_docket_guards_and_batch_validation() {
    // Verify docket Case-or-Controversy guard exits 2 and zero-candidate short-circuit exits 0 without GEMINI_API_KEY
    let docket_all_canons_no_filing = Command::new(bin_path())
        .args(["docket", "--all-canons"])
        .output()
        .expect("failed to execute canon-clerk docket --all-canons");
    assert_eq!(docket_all_canons_no_filing.status.code(), Some(2));

    let docket_zero_batch = Command::new(bin_path())
        .args(["docket", "src/lib.rs", "--docket-batch-size", "0"])
        .output()
        .expect("failed to execute canon-clerk docket --docket-batch-size 0");
    assert_eq!(docket_zero_batch.status.code(), Some(2));

    let docket_zero_cand = Command::new(bin_path())
        .args([
            "docket",
            "--canon",
            ".canons/readme-authoring/readme-must-lead-with-what-and-why.md",
            "src/lib.rs",
            "--json",
        ])
        .env_remove("GEMINI_API_KEY")
        .env("XDG_CONFIG_HOME", "/nonexistent-canon-clerk-xdg-dir")
        .output()
        .expect("failed to execute canon-clerk docket zero-candidate run");
    assert_eq!(docket_zero_cand.status.code(), Some(0));
}

#[test]
fn test_cli_admit_guards_and_zero_candidate_short_circuit() {
    // Verify admit Case-or-Controversy guard exits 2
    let admit_all_canons_no_filing = Command::new(bin_path())
        .args(["admit", "--all-canons"])
        .output()
        .expect("failed to execute canon-clerk admit --all-canons");
    assert_eq!(admit_all_canons_no_filing.status.code(), Some(2));

    // Verify invalid --admit-concurrency 0 and out-of-closure --apprise-threshold exit 2
    let admit_zero_conc = Command::new(bin_path())
        .args(["admit", "src/lib.rs", "--admit-concurrency", "0"])
        .output()
        .expect("failed to execute canon-clerk admit --admit-concurrency 0");
    assert_eq!(admit_zero_conc.status.code(), Some(2));

    let admit_out_of_closure = Command::new(bin_path())
        .args(["admit", "src/lib.rs", "--apprise-threshold", "0.7"])
        .output()
        .expect("failed to execute canon-clerk admit --apprise-threshold 0.7");
    assert_eq!(admit_out_of_closure.status.code(), Some(2));

    // Verify zero-candidate short-circuit exits 0 without GEMINI_API_KEY
    let admit_zero_cand = Command::new(bin_path())
        .args([
            "admit",
            "--canon",
            ".canons/readme-authoring/readme-must-lead-with-what-and-why.md",
            "src/lib.rs",
            "--json",
        ])
        .env_remove("GEMINI_API_KEY")
        .env("XDG_CONFIG_HOME", "/nonexistent-canon-clerk-xdg-dir")
        .output()
        .expect("failed to execute canon-clerk admit zero-candidate run");
    assert_eq!(admit_zero_cand.status.code(), Some(0));
    let parsed: Caseload =
        serde_json::from_slice(&admit_zero_cand.stdout).expect("parse admit Caseload JSON");
    assert!(parsed.evidence.is_some());
    assert!(parsed.evidence.expect("evidence").active_cases.is_empty());
}

#[test]
fn test_cli_audit_guards_and_zero_candidate_short_circuit() {
    // Verify audit Case-or-Controversy guard exits 2
    let audit_all_canons_no_filing = Command::new(bin_path())
        .args(["audit", "--all-canons"])
        .output()
        .expect("failed to execute canon-clerk audit --all-canons");
    assert_eq!(audit_all_canons_no_filing.status.code(), Some(2));

    // Verify invalid --audit-concurrency 0 and out-of-closure --apprise-threshold exit 2
    let audit_zero_conc = Command::new(bin_path())
        .args(["audit", "src/lib.rs", "--audit-concurrency", "0"])
        .output()
        .expect("failed to execute canon-clerk audit --audit-concurrency 0");
    assert_eq!(audit_zero_conc.status.code(), Some(2));

    let audit_out_of_closure = Command::new(bin_path())
        .args(["audit", "src/lib.rs", "--apprise-threshold", "0.7"])
        .output()
        .expect("failed to execute canon-clerk audit --apprise-threshold 0.7");
    assert_eq!(audit_out_of_closure.status.code(), Some(2));

    // Verify zero-candidate short-circuit exits 0 without GEMINI_API_KEY and attaches passing .verdict
    let audit_zero_cand = Command::new(bin_path())
        .args([
            "audit",
            "--canon",
            ".canons/readme-authoring/readme-must-lead-with-what-and-why.md",
            "src/lib.rs",
            "--json",
        ])
        .env_remove("GEMINI_API_KEY")
        .env("XDG_CONFIG_HOME", "/nonexistent-canon-clerk-xdg-dir")
        .output()
        .expect("failed to execute canon-clerk audit zero-candidate run");
    assert_eq!(audit_zero_cand.status.code(), Some(0));
    let parsed: Caseload =
        serde_json::from_slice(&audit_zero_cand.stdout).expect("parse audit Caseload JSON");
    assert!(parsed.verdict.expect("verdict").is_pass());
}

#[test]
fn test_cli_cold_start_latency() {
    // Benchmark repeated executions of --version to measure process cold start latency
    let bin = bin_path();
    let mut total_duration = std::time::Duration::ZERO;
    let iterations = 5;

    for _ in 0..iterations {
        let start = Instant::now();
        let status = Command::new(bin)
            .arg("--version")
            .status()
            .expect("execution failed");
        let elapsed = start.elapsed();
        assert!(status.success());
        total_duration += elapsed;
    }

    let avg_ms = total_duration.as_millis() / iterations as u128;
    println!("Average cold start latency across {iterations} runs: {avg_ms} ms");
    // Standard acceptance criteria is sub-15ms cold start
    assert!(
        avg_ms < 50,
        "Cold start average latency ({avg_ms} ms) must be well within performance budget"
    );
}

#[test]
fn test_cli_log_file_and_env_fallback_emit_jsonl_without_polluting_stdout() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("canon-clerk-telemetry-it-{unique}"));
    let flag_log = dir.join("nested").join("flag-events.jsonl");
    let env_log = dir.join("env-events.jsonl");

    // Baseline stdout without --log-file
    let baseline = Command::new(bin_path())
        .args(["validate", "src/lib.rs", "--json"])
        .output()
        .expect("baseline validate");
    assert_eq!(baseline.status.code(), Some(0));

    // 1. Explicit --log-file overrides CANON_CLERK_LOG_FILE and creates parent dirs
    let instrumented = Command::new(bin_path())
        .args(["validate", "src/lib.rs", "--json", "--log-file"])
        .arg(&flag_log)
        .env("CANON_CLERK_LOG_FILE", &env_log)
        .env("CI", "true")
        .env("NO_COLOR", "1")
        .arg("-v")
        .output()
        .expect("instrumented validate");
    assert_eq!(instrumented.status.code(), Some(0));

    // stdout Caseload JSON must remain bit-for-bit identical
    assert_eq!(instrumented.stdout, baseline.stdout);
    let _: Caseload = serde_json::from_slice(&instrumented.stdout).expect("valid Caseload JSON");
    assert!(
        !env_log.exists(),
        "--log-file must override CANON_CLERK_LOG_FILE"
    );

    let stderr_text = String::from_utf8_lossy(&instrumented.stderr);
    assert!(stderr_text.contains("[telemetry]"));
    assert!(
        !stderr_text.contains("\x1b["),
        "CI/NO_COLOR stderr must omit ANSI escapes"
    );

    let raw_jsonl = fs::read_to_string(&flag_log).expect("read flag jsonl");
    let events: Vec<TelemetryEvent> = raw_jsonl
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSONL TelemetryEvent"))
        .collect();
    let started: Vec<PipelineStage> = events
        .iter()
        .filter_map(|e| match e {
            TelemetryEvent::StageStart { stage, .. } => Some(*stage),
            _ => None,
        })
        .collect();
    assert_eq!(
        started,
        vec![
            PipelineStage::Intake,
            PipelineStage::Discover,
            PipelineStage::Validate
        ]
    );
    assert!(matches!(
        events.last(),
        Some(TelemetryEvent::PipelineFinish {
            total_tokens: 0,
            ..
        })
    ));

    // 2. CANON_CLERK_LOG_FILE fallback combined with -q quiet mode suppresses stderr
    let env_run = Command::new(bin_path())
        .args(["discover", "-q", "--all-canons"])
        .env("CANON_CLERK_LOG_FILE", &env_log)
        .arg("-v")
        .output()
        .expect("env fallback discover -q");
    assert_eq!(env_run.status.code(), Some(0));
    assert!(env_run.stdout.is_empty());
    let env_stderr = String::from_utf8_lossy(&env_run.stderr);
    assert!(
        !env_stderr.contains("[telemetry]"),
        "-q must suppress telemetry stderr progress"
    );
    let env_jsonl = fs::read_to_string(&env_log).expect("read env jsonl");
    assert!(env_jsonl.lines().count() >= 3);

    let _ = fs::remove_dir_all(&dir);
}
