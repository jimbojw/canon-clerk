use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use canon_clerk::models::Caseload;
use canon_clerk::pipeline::{
    ConfigResolver, Discover, Intake, MockProviderClient, ProbeStatus, ProviderClient,
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

    match args.format {
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

async fn run_probe(args: ProbeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let resolved = ConfigResolver::resolve(
        if args.mock {
            Some(ProviderType::Mock)
        } else {
            None
        },
        args.model,
        None,
        &env_vars,
    );

    let client = MockProviderClient::new_healthy(
        format!("{:?}", resolved.model.provider),
        resolved.model.model_name.clone(),
    );

    let probe_result = client.probe().await?;

    match args.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&probe_result)?);
        }
        OutputFormat::Human => {
            println!("=== Provider Probe ===");
            println!("Status:   {:?}", probe_result.status);
            println!("Provider: {}", probe_result.provider);
            println!("Model:    {}", probe_result.model);
            println!("Latency:  {} ms", probe_result.latency_ms);
        }
    }

    if probe_result.status != ProbeStatus::Healthy {
        std::process::exit(1);
    }

    Ok(())
}

fn collect_canons_recursive(
    dir: &std::path::Path,
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
