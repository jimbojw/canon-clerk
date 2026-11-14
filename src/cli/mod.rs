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

//! Canon Clerk CLI driving adapter and subcommand router.

pub mod admit;
pub mod apprise;
pub mod args;
pub mod audit;
mod closure;
pub mod configure;
pub mod discover;
pub mod docket;
pub mod intake;
pub mod io;
pub mod probe;
mod provenance;
pub mod runners;
pub mod telemetry;
pub mod validate;

pub use admit::{
    AdmitArgs, AdmitFormat, run_admit, run_admit_live_with_telemetry,
    run_admit_with_snapshot_and_provider, run_admit_with_telemetry,
};
pub use apprise::{
    AppriseArgs, AppriseFormat, run_apprise, run_apprise_live_with_telemetry,
    run_apprise_with_snapshot_and_provider, run_apprise_with_telemetry,
};
pub use args::{CanonSelectionArgs, CaseloadIoArgs, FilingSourceArgs, ProviderConfigArgs};
pub use audit::{
    AuditArgs, AuditFormat, run_audit, run_audit_live_with_telemetry,
    run_audit_with_snapshot_and_provider, run_audit_with_telemetry,
};
pub use configure::{
    ConfigureArgs, ConfigureFormat, run_configure, run_configure_with_env,
    run_configure_with_telemetry,
};
pub use discover::{
    DiscoverArgs, DiscoverFormat, run_discover, run_discover_in_workspace,
    run_discover_with_telemetry,
};
pub use docket::{
    DocketArgs, DocketFormat, run_docket, run_docket_live_with_telemetry,
    run_docket_with_snapshot_and_provider, run_docket_with_telemetry,
};
pub use intake::{IntakeArgs, run_intake, run_intake_with_telemetry};
pub use io::{
    ensure_single_stdin_source, read_caseload_operand, read_operand_source,
    resolve_target_operands, write_caseload_file,
};
pub use probe::{
    ProbeArgs, ProbeFormat, run_probe, run_probe_with_env_and_prober, run_probe_with_telemetry,
};
pub use provenance::StageDisposition;
pub use runners::{
    map_discover_error, resolve_branch_b_options, resolve_gemini_api_key, run_branch_a_discover,
    run_branch_a_intake, run_branch_a_validate, run_branch_b_configure,
};
pub use telemetry::{
    ENV_LOG_FILE, ENV_VERBOSE, StderrProgressSink, StderrTelemetryOptions,
    build_cli_telemetry_sink, open_jsonl_log_sink, resolve_log_file_path, resolve_verbose_flag,
};
pub use validate::{
    ValidateArgs, ValidateFormat, run_validate, run_validate_in_workspace,
    run_validate_with_telemetry,
};

use crate::probe::GeminiEndpointProber;
use crate::telemetry::{SystemTelemetryClock, TelemetrySession};
use clap::{CommandFactory, Parser, Subcommand};
use std::collections::BTreeMap;
use std::fmt;
use std::io::IsTerminal;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const ROOT_EXAMPLES: &str = "\
Examples:
  # Audit a unified diff piped on standard input across the full Caseload cascade:
  git diff origin/main | canon-clerk audit --diff -

  # Ingest explicit target paths or globs:
  canon-clerk intake src/lib.rs 'src/**/*.rs'

  # Record out-of-band JSONL stage telemetry alongside Caseload JSON output:
  canon-clerk validate --all-canons --log-file telemetry.jsonl --json";

/// Canon Clerk CLI command-line argument parser.
#[derive(Parser, Debug, Clone, Default, PartialEq, Eq)]
#[command(
    name = "canon-clerk",
    author = "PAIR-code",
    version = env!("CARGO_PKG_VERSION"),
    about = "Automated review gate enforcing declarative engineering rule packs",
    long_about = "Canon Clerk is an automated review gate that enforces declarative engineering rule packs (canons) and repository-specific invariants.",
    after_help = ROOT_EXAMPLES,
    propagate_version = true,
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = false
)]
pub struct Cli {
    /// Optional Caseload DAG stage subcommand.
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Primary positional operational targets (file paths, directories, or globs).
    #[arg(
        value_name = "TARGETS",
        help = "Target file paths, directories, or globs to evaluate"
    )]
    pub targets: Vec<PathBuf>,

    /// Diagnostic verbose flag with positive polarity (fallback env: `CANON_CLERK_VERBOSE`).
    #[arg(
        short = 'v',
        long = "verbose",
        global = true,
        help_heading = "Logging & Diagnostics",
        help = "Enable verbose diagnostic output (fallback env: CANON_CLERK_VERBOSE)"
    )]
    pub verbose: bool,

    /// Write structured JSON Lines (`JSONL`) telemetry events to disk (fallback env: `CANON_CLERK_LOG_FILE`).
    #[arg(
        long = "log-file",
        value_name = "PATH",
        global = true,
        help_heading = "Logging & Diagnostics",
        help = "Write structured JSONL telemetry events to file (fallback env: CANON_CLERK_LOG_FILE)"
    )]
    pub log_file: Option<PathBuf>,
}

/// Top-level Caseload DAG stage subcommands.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum Commands {
    /// Universal front door: ingest diffs, target paths, design intent, or PR metadata.
    Intake(IntakeArgs),
    /// Multi-plane relevance sieve: identify candidate canons and prune un-inspected exhibits.
    Discover(DiscoverArgs),
    /// Statutory validity gate: statically lint candidate canon AST and frontmatter schemas.
    Validate(ValidateArgs),
    /// Operational environment normalizer: verify credentials and normalize model specifiers.
    Configure(ConfigureArgs),
    /// Diagnostic network health check: verify live connectivity and latency of configured endpoints.
    Probe(ProbeArgs),
    /// Prospective statutory notice: determine governing canons for planned design intent and target scope.
    Apprise(AppriseArgs),
    /// Macro jurisdiction triage: screen candidate canons to open active cases on the docket.
    Docket(DocketArgs),
    /// Micro evidentiary triage: screen candidate file and metadata exhibits per active case.
    Admit(AdmitArgs),
    /// Judicial adjudication: evaluate admitted exhibits against canon statutes and render verdicts.
    Audit(AuditArgs),
}

impl Commands {
    const fn is_quiet(&self) -> bool {
        match self {
            Self::Discover(args) => args.quiet,
            Self::Validate(args) => args.quiet,
            Self::Intake(_)
            | Self::Configure(_)
            | Self::Probe(_)
            | Self::Apprise(_)
            | Self::Docket(_)
            | Self::Admit(_)
            | Self::Audit(_) => false,
        }
    }
}

/// Structured CLI error carrying a standardized POSIX exit code (`1` runtime/predicate, `2` usage/input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    exit_code: i32,
    message: String,
    silent: bool,
}

impl CliError {
    /// Constructs a usage or input error that terminates with exit code `2`.
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            exit_code: 2,
            message: message.into(),
            silent: false,
        }
    }

    /// Constructs a runtime execution error that terminates with exit code `1`.
    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            exit_code: 1,
            message: message.into(),
            silent: false,
        }
    }

    /// Constructs a silent non-zero exit (used for `-q` / `--quiet` predicate mode).
    pub fn silent(exit_code: i32) -> Self {
        Self {
            exit_code,
            message: String::new(),
            silent: true,
        }
    }

    /// Returns the shell exit code associated with this error.
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns `true` if this error should exit without printing an `error:` prefix to stderr.
    pub fn is_silent(&self) -> bool {
        self.silent
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

impl Cli {
    /// Renders the standardized help screen as a String.
    pub fn render_help() -> String {
        let mut cmd = Self::command();
        cmd.render_help().to_string()
    }
}

/// Initializes structured logging directed exclusively to stderr.
pub fn init_logging(verbose: bool) {
    let env_map: BTreeMap<String, String> = std::env::vars().collect();
    let stderr_opts =
        StderrTelemetryOptions::from_env(std::io::stderr().is_terminal(), verbose, false, &env_map);
    init_logging_with_ansi(verbose, stderr_opts.use_ansi_color());
}

fn init_logging_with_ansi(verbose: bool, use_ansi: bool) {
    let env_filter = if verbose {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(use_ansi)
                .compact(),
        )
        .try_init()
        .ok();
}

fn current_workspace_dir() -> Result<PathBuf, CliError> {
    std::env::current_dir().map_err(|err| {
        CliError::runtime(format!(
            "Failed to determine current working directory: {err}"
        ))
    })
}

/// Executes the CLI application logic and routes subcommands.
pub fn run(cli: Cli) -> Result<(), CliError> {
    let env_map: BTreeMap<String, String> = std::env::vars().collect();
    let effective_verbose = resolve_verbose_flag(cli.verbose, &env_map)?;
    let log_file_path = resolve_log_file_path(cli.log_file.as_deref(), &env_map)?;
    let subcommand_quiet = cli.command.as_ref().is_some_and(Commands::is_quiet);

    let stderr_opts = StderrTelemetryOptions::from_env(
        std::io::stderr().is_terminal(),
        effective_verbose,
        subcommand_quiet,
        &env_map,
    );
    init_logging_with_ansi(effective_verbose, stderr_opts.use_ansi_color());

    tracing::debug!("Initialized Canon Clerk CLI runtime");

    if let Some(command) = &cli.command {
        let sink = build_cli_telemetry_sink(log_file_path.as_deref(), stderr_opts)?;
        let clock = SystemTelemetryClock::new();
        let session = TelemetrySession::new(&sink, &clock);

        let mut stdin = std::io::stdin().lock();
        let mut stdout = std::io::stdout().lock();
        dispatch_subcommand(command, &mut stdin, &mut stdout, &env_map, &session)?;
        return Ok(());
    }

    if cli.targets.is_empty() {
        Cli::command()
            .print_help()
            .map_err(|err| CliError::runtime(err.to_string()))?;
        println!();
        return Ok(());
    }

    tracing::info!("Received {} target operand(s)", cli.targets.len());
    Ok(())
}

fn dispatch_subcommand(
    command: &Commands,
    stdin: &mut dyn std::io::Read,
    stdout: &mut dyn std::io::Write,
    env_map: &BTreeMap<String, String>,
    session: &TelemetrySession<'_>,
) -> Result<(), CliError> {
    match command {
        Commands::Intake(args) => {
            run_intake_with_telemetry(args, stdin, stdout, session)?;
        }
        Commands::Discover(args) => {
            let cwd = current_workspace_dir()?;
            run_discover_with_telemetry(args, stdin, stdout, &cwd, session)?;
        }
        Commands::Validate(args) => {
            let cwd = current_workspace_dir()?;
            run_validate_with_telemetry(args, stdin, stdout, &cwd, session)?;
        }
        Commands::Configure(args) => {
            run_configure_with_telemetry(args, stdin, stdout, env_map, session)?;
        }
        Commands::Probe(args) => {
            let prober = GeminiEndpointProber::new();
            run_probe_with_telemetry(args, stdin, stdout, env_map, &prober, session)?;
        }
        Commands::Apprise(args) => {
            run_apprise_live_with_telemetry(args, stdin, stdout, session)?;
        }
        Commands::Docket(args) => {
            run_docket_live_with_telemetry(args, stdin, stdout, session)?;
        }
        Commands::Admit(args) => {
            run_admit_live_with_telemetry(args, stdin, stdout, session)?;
        }
        Commands::Audit(args) => {
            run_audit_live_with_telemetry(args, stdin, stdout, session)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod argument_parsing {
        use super::*;

        #[test]
        fn test_cli_parse_empty() {
            let cli =
                Cli::try_parse_from(["canon-clerk"]).expect("parsing empty args should succeed");
            assert!(cli.command.is_none());
            assert!(cli.targets.is_empty());
            assert!(!cli.verbose);
            assert!(cli.log_file.is_none());
        }

        #[test]
        fn test_cli_parse_targets_positional() {
            let cli = Cli::try_parse_from(["canon-clerk", "src/lib.rs", "SPEC.md"])
                .expect("parsing positionals should succeed");
            assert_eq!(cli.targets.len(), 2);
            assert_eq!(cli.targets[0], PathBuf::from("src/lib.rs"));
            assert_eq!(cli.targets[1], PathBuf::from("SPEC.md"));
            assert!(!cli.verbose);
        }

        #[test]
        fn test_cli_parse_global_log_file_before_and_after_subcommand() {
            let before = Cli::try_parse_from([
                "canon-clerk",
                "--log-file",
                "before.jsonl",
                "intake",
                "src/lib.rs",
            ])
            .expect("global --log-file before subcommand");
            assert_eq!(before.log_file, Some(PathBuf::from("before.jsonl")));

            let after = Cli::try_parse_from([
                "canon-clerk",
                "intake",
                "src/lib.rs",
                "--log-file",
                "after.jsonl",
                "-v",
            ])
            .expect("global --log-file after subcommand");
            assert_eq!(after.log_file, Some(PathBuf::from("after.jsonl")));
            assert!(after.verbose);
        }

        #[test]
        fn test_cli_parse_intake_subcommand() {
            let cli = Cli::try_parse_from([
                "canon-clerk",
                "intake",
                "--diff",
                "-",
                "--pr-title",
                "feat: test",
                "--json",
            ])
            .expect("parsing intake subcommand should succeed");

            match cli.command {
                Some(Commands::Intake(args)) => {
                    assert_eq!(args.filing.diff.as_deref(), Some("-"));
                    assert_eq!(args.filing.pr_title.as_deref(), Some("feat: test"));
                    assert!(args.io.json);
                }
                Some(
                    Commands::Discover(_)
                    | Commands::Validate(_)
                    | Commands::Configure(_)
                    | Commands::Probe(_)
                    | Commands::Apprise(_)
                    | Commands::Docket(_)
                    | Commands::Admit(_)
                    | Commands::Audit(_),
                )
                | None => {
                    panic!("expected Intake subcommand")
                }
            }
        }

        #[test]
        fn test_cli_parse_verbose_short_and_long() {
            let cli_short = Cli::try_parse_from(["canon-clerk", "-v"]).expect("-v should succeed");
            assert!(cli_short.verbose);

            let cli_long = Cli::try_parse_from(["canon-clerk", "--verbose"])
                .expect("--verbose should succeed");
            assert!(cli_long.verbose);
        }

        #[test]
        fn test_cli_parse_invalid_flag_yields_usage_error() {
            let err = Cli::try_parse_from(["canon-clerk", "--unrecognized-option"])
                .expect_err("unrecognized flag must fail");
            assert!(err.use_stderr());
            assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    mod help_and_metadata {
        use super::*;

        #[test]
        fn test_cli_help_screen_groups_options_by_concern() {
            let help = Cli::render_help();
            assert!(
                help.contains("Logging & Diagnostics:"),
                "help screen must contain 'Logging & Diagnostics' group heading"
            );
            assert!(
                help.contains("-v, --verbose"),
                "help screen must document verbose flag"
            );
            assert!(
                help.contains("--log-file <PATH>"),
                "help screen must document --log-file flag"
            );
            assert!(
                help.contains("CANON_CLERK_LOG_FILE"),
                "help screen must document CANON_CLERK_LOG_FILE fallback"
            );
            assert!(
                help.contains("TARGETS"),
                "help screen must document positional TARGETS"
            );
            assert!(
                help.contains("intake"),
                "help screen must document intake subcommand"
            );
        }

        #[test]
        fn test_cli_version_matches_cargo_pkg() {
            let cmd = Cli::command();
            assert_eq!(cmd.get_version(), Some(env!("CARGO_PKG_VERSION")));
        }
    }
}
