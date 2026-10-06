use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use canon_clerk::models::{CanonAst, Caseload, ChangeType, FileArtifact};
use canon_clerk::pipeline::{
    AdmitRunner, AnyProviderClient, AppriseRunner, AuditRunner, ConfigResolver, Discover,
    DocketRunner, GoogleProviderClient, Intake, MockProviderClient, ProbeStatus, ProviderClient,
    ProviderType, Validate,
};

#[derive(Parser, Debug)]
#[command(
    name = "canon-clerk",
    author = "PAIR-code",
    version = env!("CARGO_PKG_VERSION"),
    about = "Automated engineering rule pack review gate",
    propagate_version = true
)]
struct Cli {
    #[arg(short, long, global = true, help = "Enable verbose diagnostic logging")]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Validate canons and check Caseload invariants")]
    Validate(ValidateArgs),

    #[command(about = "Probe LLM provider connectivity")]
    Probe(ProbeArgs),

    #[command(about = "Perform Phase 2 macro triage docketing of candidate canons")]
    Docket(DocketArgs),

    #[command(about = "Perform Phase 2 micro triage exhibit admissibility")]
    Admit(AdmitArgs),

    #[command(about = "Execute full end-to-end Caseload DAG adjudication audit")]
    Audit(AuditArgs),

    #[command(about = "Perform prospective statutory apprisal of design intent and target paths")]
    Apprise(AppriseArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Args, Debug)]
struct ValidateArgs {
    #[arg(
        default_value = ".canons",
        help = "Path to canon file or directory to validate"
    )]
    path: PathBuf,

    #[arg(long, help = "Optional diff file to intake for trigger evaluation")]
    diff_file: Option<PathBuf>,

    #[arg(long, help = "Output unadorned JSON to stdout")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "human",
        help = "Output formatting"
    )]
    format: OutputFormat,
}

#[derive(Args, Debug)]
struct ProbeArgs {
    #[arg(long, help = "Use offline mock provider")]
    mock: bool,

    #[arg(long, help = "Model name override")]
    model: Option<String>,

    #[arg(long, help = "Output unadorned JSON to stdout")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "human",
        help = "Output formatting"
    )]
    format: OutputFormat,
}

#[derive(Args, Debug)]
struct DocketArgs {
    #[arg(
        default_value = ".canons",
        help = "Path to canon file or directory to evaluate"
    )]
    path: PathBuf,

    #[arg(long, help = "Optional diff file to intake (reads stdin or git diff if not specified)")]
    diff_file: Option<PathBuf>,

    #[arg(long, help = "Use offline mock provider")]
    mock: bool,

    #[arg(long, help = "Model name override")]
    model: Option<String>,

    #[arg(long, help = "Output unadorned JSON to stdout")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "human",
        help = "Output formatting"
    )]
    format: OutputFormat,
}

#[derive(Args, Debug)]
struct AdmitArgs {
    #[arg(
        default_value = ".canons",
        help = "Path to canon file or directory to evaluate"
    )]
    path: PathBuf,

    #[arg(long, help = "Optional diff file to intake (reads stdin or git diff if not specified)")]
    diff_file: Option<PathBuf>,

    #[arg(long, help = "Use offline mock provider")]
    mock: bool,

    #[arg(long, help = "Model name override")]
    model: Option<String>,

    #[arg(long, help = "Output unadorned JSON to stdout")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "human",
        help = "Output formatting"
    )]
    format: OutputFormat,
}

#[derive(Args, Debug)]
struct AuditArgs {
    #[arg(
        default_value = ".canons",
        help = "Path to canon file or directory to evaluate"
    )]
    path: PathBuf,

    #[arg(long, help = "Optional diff file to intake (reads stdin or git diff if not specified)")]
    diff_file: Option<PathBuf>,

    #[arg(long, help = "Use offline mock provider")]
    mock: bool,

    #[arg(long, help = "Model name override")]
    model: Option<String>,

    #[arg(long, help = "Output unadorned JSON to stdout")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "human",
        help = "Output formatting"
    )]
    format: OutputFormat,
}

#[derive(Args, Debug)]
struct AppriseArgs {
    #[arg(help = "Prospective target paths or directory containing canons")]
    paths: Vec<PathBuf>,

    #[arg(long, help = "Prospective design intent or RFC text (use '-' for stdin)")]
    intent: Option<String>,

    #[arg(
        long,
        default_value = "0.5",
        help = "Minimum apprisal salience threshold [0.0, 1.0]"
    )]
    threshold: f32,

    #[arg(long, help = "Use offline mock provider")]
    mock: bool,

    #[arg(long, help = "Model name override")]
    model: Option<String>,

    #[arg(long, help = "Output unadorned JSON to stdout")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "human",
        help = "Output formatting"
    )]
    format: OutputFormat,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let env_filter = if cli.verbose {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().compact())
        .try_init()
        .ok();

    match cli.command {
        Some(Commands::Validate(args)) => run_validate(args),
        Some(Commands::Probe(args)) => run_probe(args).await,
        Some(Commands::Docket(args)) => run_docket(args).await,
        Some(Commands::Admit(args)) => run_admit(args).await,
        Some(Commands::Audit(args)) => run_audit(args).await,
        Some(Commands::Apprise(args)) => run_apprise(args).await,
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn run_validate(args: ValidateArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut caseload = Caseload::new("cli-session");

    // Intake diff if provided
    if let Some(diff_path) = args.diff_file {
        if diff_path.exists() {
            let diff_content = std::fs::read_to_string(diff_path)?;
            let artifacts = Intake::ingest_diff(&diff_content);
            for art in artifacts {
                caseload.add_artifact(art);
            }
        }
    }

    // Discover canon files
    let canon_paths = if args.path.is_file() {
        vec![args.path]
    } else if args.path.is_dir() {
        let mut paths = Vec::new();
        collect_canons_recursive(&args.path, &mut paths)?;
        paths.sort();
        paths
    } else {
        Vec::new()
    };

    let mut errors = Vec::new();

    for cp in &canon_paths {
        let content = std::fs::read_to_string(cp)?;
        let path_str = cp.to_string_lossy();
        match Validate::parse_and_validate(&path_str, &content) {
            Ok(ast) => caseload.add_canon(ast),
            Err(e) => errors.push(format!("{}: {}", path_str, e)),
        }
    }

    let active_ids = Discover::discover_active_canons(&caseload.canons, &caseload.artifacts);
    for id in active_ids {
        caseload.activate_canon(id);
    }

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&serde_json::json!({
                "valid": errors.is_empty(),
                "errors": errors,
                "summary": caseload.summary(),
            }))?;
            println!("{}", json);
        }
        OutputFormat::Human => {
            println!("=== Canon Clerk Validation ===");
            println!("Discovered canons: {}", caseload.canons.len());
            println!("Active canons:     {}", caseload.active_canon_ids.len());
            if !errors.is_empty() {
                println!("\nValidation Errors ({}):", errors.len());
                for err in &errors {
                    println!("  - {}", err);
                }
            } else {
                println!("\nAll discovered canons passed structural validation.");
            }
        }
    }

    if !errors.is_empty() {
        std::process::exit(1);
    }

    Ok(())
}

fn resolve_client(mock: bool, model: Option<String>) -> Result<AnyProviderClient, Box<dyn std::error::Error>> {
    let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let resolved = ConfigResolver::resolve(
        if mock {
            Some(ProviderType::Mock)
        } else {
            None
        },
        model,
        None,
        &env_vars,
    );

    if mock || resolved.model.provider == ProviderType::Mock {
        Ok(AnyProviderClient::Mock(MockProviderClient::new_healthy(
            format!("{}", resolved.model.provider),
            resolved.model.model_name.clone(),
        )))
    } else {
        match resolved.model.provider {
            ProviderType::Gemini => {
                if let Some(key) = resolved.credentials.api_key {
                    Ok(AnyProviderClient::Google(GoogleProviderClient::new(
                        key,
                        resolved.credentials.api_endpoint,
                        resolved.model.model_name.clone(),
                    )))
                } else {
                    eprintln!("Error: Missing API key for Google Gemini provider.");
                    eprintln!("Please configure it in ~/.config/canon-clerk/config.json (.providers.google.apiKey) or export GEMINI_API_KEY.");
                    std::process::exit(1);
                }
            }
            other => {
                eprintln!("Error: Provider '{}' live client not configured. Use --mock or configure Google Gemini.", other);
                std::process::exit(1);
            }
        }
    }
}

fn resolve_diff(diff_file: Option<PathBuf>) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(diff_path) = diff_file {
        if diff_path.exists() {
            return Ok(std::fs::read_to_string(diff_path)?);
        } else {
            return Err(format!("Diff file not found: {}", diff_path.display()).into());
        }
    }

    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        if !buffer.trim().is_empty() {
            return Ok(buffer);
        }
    }

    // Check unstaged git diff
    if let Ok(output) = std::process::Command::new("git").args(["diff"]).output() {
        let diff_str = String::from_utf8_lossy(&output.stdout).to_string();
        if !diff_str.trim().is_empty() {
            return Ok(diff_str);
        }
    }

    // Check staged git diff
    if let Ok(output) = std::process::Command::new("git").args(["diff", "--cached"]).output() {
        let diff_str = String::from_utf8_lossy(&output.stdout).to_string();
        if !diff_str.trim().is_empty() {
            return Ok(diff_str);
        }
    }

    // Check diff against merge-base with upstream
    if let Ok(output) = std::process::Command::new("git")
        .args(["merge-base", "HEAD", "@{upstream}"])
        .output()
    {
        let base_commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !base_commit.is_empty() {
            if let Ok(diff_output) = std::process::Command::new("git")
                .args(["diff", &base_commit])
                .output()
            {
                let diff_str = String::from_utf8_lossy(&diff_output.stdout).to_string();
                if !diff_str.trim().is_empty() {
                    return Ok(diff_str);
                }
            }
        }
    }

    // Fallback to previous commit diff
    if let Ok(output) = std::process::Command::new("git").args(["diff", "HEAD~1"]).output() {
        let diff_str = String::from_utf8_lossy(&output.stdout).to_string();
        if !diff_str.trim().is_empty() {
            return Ok(diff_str);
        }
    }

    Ok(String::new())
}

fn load_canons(path: &Path) -> Result<Vec<CanonAst>, Box<dyn std::error::Error>> {
    let canon_paths = if path.is_file() {
        vec![path.to_path_buf()]
    } else if path.is_dir() {
        let mut paths = Vec::new();
        collect_canons_recursive(path, &mut paths)?;
        paths.sort();
        paths
    } else {
        Vec::new()
    };

    let mut canons = Vec::new();
    for cp in &canon_paths {
        let content = std::fs::read_to_string(cp)?;
        let path_str = cp.to_string_lossy();
        if let Ok(ast) = Validate::parse_and_validate(&path_str, &content) {
            canons.push(ast);
        }
    }
    Ok(canons)
}

fn summarize_diff(diff_text: &str, artifacts: &[FileArtifact]) -> String {
    if diff_text.is_empty() {
        return "(No diff provided; evaluating overall repository context)".to_string();
    }

    let mut summary = String::new();
    let (mut active, deleted): (Vec<&FileArtifact>, Vec<&FileArtifact>) = artifacts
        .iter()
        .partition(|a| a.change_type != ChangeType::Deleted);

    if active.is_empty() {
        active = deleted;
    }

    summary.push_str(&format!(
        "Modified/added active files ({} active, {} total in diff):\n",
        active.len(),
        artifacts.len()
    ));
    for art in active.iter().take(40) {
        summary.push_str(&format!("- {} ({:?})\n", art.path, art.change_type));
    }
    if active.len() > 40 {
        summary.push_str(&format!("- ... and {} more files\n", active.len() - 40));
    }

    summary.push_str("\nDiff excerpt:\n");
    let mut lines_count = 0;
    for art in &active {
        if let Some(diff) = &art.diff {
            summary.push_str(&format!("\n--- File: {} ---\n", art.path));
            for line in diff.lines() {
                summary.push_str(line);
                summary.push('\n');
                lines_count += 1;
                if lines_count >= 150 {
                    break;
                }
            }
        }
        if lines_count >= 150 {
            break;
        }
    }

    summary
}

async fn run_probe(args: ProbeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let client = resolve_client(args.mock, args.model)?;
    let probe_result = client.probe().await?;

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&probe_result)?);
        }
        OutputFormat::Human => {
            println!("=== Provider Probe ===");
            println!("Status:   {:?}", probe_result.status);
            println!("Provider: {}", probe_result.provider);
            println!("Model:    {}", probe_result.model);
            println!("Latency:  {} ms", probe_result.latency_ms);
            if let Some(msg) = probe_result.message {
                println!("Message:  {}", msg);
            }
        }
    }

    if probe_result.status != ProbeStatus::Healthy {
        std::process::exit(1);
    }

    Ok(())
}

async fn run_docket(args: DocketArgs) -> Result<(), Box<dyn std::error::Error>> {
    let diff_text = resolve_diff(args.diff_file)?;
    let client = resolve_client(args.mock, args.model)?;
    let canons = load_canons(&args.path)?;

    let artifacts = Intake::ingest_diff(&diff_text);

    let candidate_canons: Vec<&CanonAst> = if !artifacts.is_empty() {
        let active_ids = Discover::discover_active_canons(&canons, &artifacts);
        let filtered: Vec<&CanonAst> = canons
            .iter()
            .filter(|c| active_ids.contains(&c.canon_id()))
            .collect();
        if filtered.is_empty() {
            canons.iter().collect()
        } else {
            filtered
        }
    } else {
        canons.iter().collect()
    };

    let diff_summary = summarize_diff(&diff_text, &artifacts);

    let assessments = DocketRunner::execute_docket(&candidate_canons, &diff_summary, &client).await?;
    let docketed_count = assessments.iter().filter(|a| a.is_docketed()).count();

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "total_evaluated": assessments.len(),
                "docketed_count": docketed_count,
                "assessments": assessments,
            }))?);
        }
        OutputFormat::Human => {
            println!("=== Canon Clerk Docket Screening ===");
            println!("Evaluated Canons: {}", assessments.len());
            println!("Docketed:         {}", docketed_count);
            println!("Dismissed:        {}", assessments.len() - docketed_count);
            println!();

            for a in &assessments {
                let badge = if a.is_docketed() { "[DOCKETED]" } else { "[DISMISSED]" };
                println!("{} {} (Score: {:.2})", badge, a.canon_path, a.colorability_score);
                println!("    Summary: {}", a.colorability_summary);
            }
        }
    }

    Ok(())
}

async fn run_admit(args: AdmitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let diff_text = resolve_diff(args.diff_file)?;
    let client = resolve_client(args.mock, args.model)?;
    let canons = load_canons(&args.path)?;

    let artifacts = Intake::ingest_diff(&diff_text);
    if artifacts.is_empty() {
        println!("No file artifacts found in diff to evaluate for admissibility.");
        return Ok(());
    }

    let diff_summary = summarize_diff(&diff_text, &artifacts);
    let candidate_canons: Vec<&CanonAst> = canons.iter().collect();

    let assessments = DocketRunner::execute_docket(&candidate_canons, &diff_summary, &client).await?;
    let docketed_paths: std::collections::HashSet<String> = assessments
        .iter()
        .filter(|a| a.is_docketed())
        .map(|a| a.canon_path.clone())
        .collect();

    let docketed_canons: Vec<&CanonAst> = canons
        .iter()
        .filter(|c| docketed_paths.contains(&c.path))
        .collect();

    let artifact_refs: Vec<&FileArtifact> = artifacts.iter().collect();
    let mut all_exhibits = Vec::new();

    for canon in &docketed_canons {
        let exhibits = AdmitRunner::execute_admit(canon, &artifact_refs, &client).await?;
        all_exhibits.extend(exhibits);
    }

    let admitted_count = all_exhibits.iter().filter(|e| e.is_admitted()).count();

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "docketed_canons": docketed_canons.len(),
                "total_exhibits_evaluated": all_exhibits.len(),
                "admitted_count": admitted_count,
                "exhibits": all_exhibits,
            }))?);
        }
        OutputFormat::Human => {
            println!("=== Canon Clerk Exhibit Admissibility ===");
            println!("Docketed Canons: {}", docketed_canons.len());
            println!("Evaluated Files: {}", artifacts.len());
            println!("Admitted:        {}", admitted_count);
            println!("Excluded:        {}", all_exhibits.len() - admitted_count);
            println!();

            for e in &all_exhibits {
                let badge = if e.is_admitted() { "[ADMITTED]" } else { "[EXCLUDED]" };
                println!(
                    "{} Canon: `{}` -> File: `{}` (Score: {:.2})",
                    badge, e.canon_path, e.file_path, e.admissibility_score
                );
                println!("    Summary: {}", e.admissibility_summary);
            }
        }
    }

    Ok(())
}

async fn run_audit(args: AuditArgs) -> Result<(), Box<dyn std::error::Error>> {
    let diff_text = resolve_diff(args.diff_file)?;
    let client = resolve_client(args.mock, args.model)?;
    let canons = load_canons(&args.path)?;

    let artifacts = Intake::ingest_diff(&diff_text);
    if artifacts.is_empty() {
        println!("No file artifacts found in diff to audit.");
        return Ok(());
    }

    let diff_summary = summarize_diff(&diff_text, &artifacts);
    let candidate_canons: Vec<&CanonAst> = canons.iter().collect();

    let assessments = DocketRunner::execute_docket(&candidate_canons, &diff_summary, &client).await?;
    let docketed_paths: std::collections::HashSet<String> = assessments
        .iter()
        .filter(|a| a.is_docketed())
        .map(|a| a.canon_path.clone())
        .collect();

    let docketed_canons: Vec<&CanonAst> = canons
        .iter()
        .filter(|c| docketed_paths.contains(&c.path))
        .collect();

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };

    if docketed_canons.is_empty() {
        match format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "is_passing": true,
                    "summary": "No canons docketed for this change.",
                    "adjudications": [],
                }))?);
            }
            OutputFormat::Human => {
                println!("=== Canon Clerk Audit Adjudication ===");
                println!("No canons have active jurisdiction (all dismissed at docket).");
                println!("Outcome: PASS (No applicable constraints violated)");
            }
        }
        return Ok(());
    }

    let artifact_refs: Vec<&FileArtifact> = artifacts.iter().collect();
    let mut adjudications = Vec::new();

    for canon in &docketed_canons {
        let exhibits = AdmitRunner::execute_admit(canon, &artifact_refs, &client).await?;
        let admitted_paths: std::collections::HashSet<String> = exhibits
            .iter()
            .filter(|e| e.is_admitted())
            .map(|e| e.file_path.clone())
            .collect();

        let mut admitted_artifacts: Vec<FileArtifact> = artifacts
            .iter()
            .filter(|a| admitted_paths.contains(&a.path))
            .cloned()
            .collect();

        if admitted_artifacts.is_empty() {
            continue;
        }

        // Enrich admitted exhibits with full file content if available on disk
        for art in &mut admitted_artifacts {
            if art.content.is_none() {
                if let Ok(content) = std::fs::read_to_string(&art.path) {
                    if content.len() < 120_000 {
                        art.content = Some(content);
                    }
                }
            }
        }

        let admitted_refs: Vec<&FileArtifact> = admitted_artifacts.iter().collect();
        let adjudication = AuditRunner::execute_audit(canon, &admitted_refs, &client).await?;
        adjudications.push(adjudication);
    }

    let is_passing = adjudications.iter().all(|a| a.is_passing());
    let failing_count = adjudications.iter().filter(|a| !a.is_passing()).count();

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "is_passing": is_passing,
                "total_adjudicated": adjudications.len(),
                "failing_count": failing_count,
                "adjudications": adjudications,
            }))?);
        }
        OutputFormat::Human => {
            println!("=== Canon Clerk Audit Adjudication ===");
            println!("Docketed Canons Evaluated: {}", docketed_canons.len());
            println!("Statutes Adjudicated:      {}", adjudications.len());
            if adjudications.is_empty() {
                println!("Note:                      No files were admitted as evidence for the docketed canons.");
            }
            println!("Outcome:                   {}", if is_passing { "PASS" } else { "FAIL" });
            println!();

            for adj in &adjudications {
                let badge = if adj.is_passing() { "[PASS]" } else { "[FAIL]" };
                println!("{} Canon: `{}` (Score: {:.2})", badge, adj.canon_path, adj.compliance_score);
                println!("    Decree: {}", adj.compliance_summary);

                if !adj.annotations.is_empty() {
                    println!("    Violations:");
                    for ann in &adj.annotations {
                        let line_str = ann.line.map(|l| format!(":{}", l)).unwrap_or_default();
                        println!("      - {}{}: [{}] {}", ann.file_path, line_str, ann.severity, ann.message);
                    }
                }
                println!();
            }

            if is_passing {
                println!("All active canons satisfied.");
            } else {
                println!("Audit FAILED: {} canon violation(s) detected.", failing_count);
            }
        }
    }

    if !is_passing {
        std::process::exit(1);
    }

    Ok(())
}

async fn run_apprise(args: AppriseArgs) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{IsTerminal, Read};

    // 1. Missing Input Source Guard (Naked Invocation)
    let is_stdin_piped = !std::io::stdin().is_terminal();
    let is_stdin_requested = args.intent.as_deref() == Some("-");
    let has_intent = args.intent.is_some() && !is_stdin_requested;
    let has_paths = !args.paths.is_empty();

    if !has_intent && !is_stdin_requested && !has_paths && !is_stdin_piped {
        eprintln!("error: No design intent or target paths provided for apprise.");
        eprintln!("  Hint: Provide an intent ('--intent <text>'), pipe a specification via standard input ('-'),");
        eprintln!("        or specify prospective target paths.");
        std::process::exit(2);
    }

    // 2. Resolve Design Intent
    let intent_text = if is_stdin_requested || (is_stdin_piped && args.intent.is_none()) {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer.trim().to_string()
    } else if let Some(ref text) = args.intent {
        text.clone()
    } else {
        format!(
            "Prospective changes to target paths: {}",
            args.paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        )
    };

    // 3. Resolve Canons and Prospective Target Paths
    let default_canons_dir = PathBuf::from(".canons");
    let mut canons = Vec::new();
    let mut prospective_artifacts = Vec::new();

    let mut target_paths = Vec::new();
    for p in &args.paths {
        if p.starts_with(".canons") || p.extension().and_then(|s| s.to_str()) == Some("md") {
            let loaded = load_canons(p)?;
            canons.extend(loaded);
        } else {
            target_paths.push(p.clone());
        }
    }

    if canons.is_empty() && default_canons_dir.exists() {
        canons = load_canons(&default_canons_dir)?;
    }

    for p in target_paths {
        prospective_artifacts.push(FileArtifact::new(
            p.to_string_lossy(),
            canon_clerk::models::ChangeType::Modified,
        ));
    }

    let candidate_canons: Vec<&CanonAst> = if !prospective_artifacts.is_empty() {
        let active_ids = Discover::discover_active_canons(&canons, &prospective_artifacts);
        canons
            .iter()
            .filter(|c| active_ids.contains(&c.canon_id()))
            .collect()
    } else {
        canons.iter().collect()
    };

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };

    // 4. Zero Candidate Canon Short-Circuit
    if candidate_canons.is_empty() {
        match format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "assessments": {}
                }))?);
            }
            OutputFormat::Human => {
                println!("0 canons triggered by prospective target scope; no applicable constraints.");
            }
        }
        return Ok(());
    }

    // 5. Execute Statutory Apprisal Screening
    let client = resolve_client(args.mock, args.model)?;
    let apprisal = AppriseRunner::execute_apprise(
        &candidate_canons,
        &intent_text,
        args.threshold,
        &client,
    )
    .await?;

    // 6. Render Output
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "assessments": apprisal.assessments
            }))?);
        }
        OutputFormat::Human => {
            println!("=== Canon Clerk Statutory Apprisal ===");
            println!("Candidate Canons: {}", candidate_canons.len());
            println!("Applicable:       {}", apprisal.applicable_count());
            println!("Dismissed:        {}", apprisal.dismissed_count());
            println!();

            for (path, assessment) in &apprisal.assessments {
                let badge = if assessment.is_applicable() {
                    "[APPLICABLE]"
                } else {
                    "[DISMISSED]"
                };
                println!("{} {} (Score: {:.2})", badge, path, assessment.apprisal_score);
                println!("    Summary: {}", assessment.apprisal_summary);
            }
        }
    }

    Ok(())
}

fn collect_canons_recursive(
    dir: &Path,
    paths: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                collect_canons_recursive(&p, paths)?;
            } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
                paths.push(p);
            }
        }
    }
    Ok(())
}
