use clap::{Parser, Subcommand};
use log::{debug, info};
use std::path::Path;
use std::time::Instant;

mod offload;
use offload::CargoOffload;

mod util;
use util::*;

#[derive(Parser)]
#[command(name = "offload")]
#[command(about = "A CLI tool for remote Rust compilation")]
#[command(disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// SSH host (user@hostname or just hostname)
    #[arg(short, long, global = true)]
    host: Option<String>,

    /// SSH port (defaults to 22, can also be specified in CARGO_OFFLOAD_HOST)
    #[arg(short, long, global = true)]
    port: Option<u16>,

    /// Target triple (defaults to x86_64-unknown-linux-gnu)
    #[arg(long, global = true)]
    target: Option<String>,

    /// Environment variables to pass to the remote cargo command (e.g. CC=gcc-13)
    #[arg(short = 'e', long = "env", global = true)]
    env_vars: Vec<String>,

    /// Copy all artifacts from target directory (including deps, build, etc.)
    #[arg(long = "copy-all-artifacts", global = true)]
    copy_all_artifacts: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Build the project on remote and copy binaries back
    Build {
        /// All arguments to pass to cargo build
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Build on remote, copy binaries, and run locally
    Run {
        /// Arguments to pass to cargo build
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },
    #[command(name = "run-local")]
    RunLocal {
        /// Arguments to pass to cargo build
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Run tests on remote
    Test {
        /// All arguments to pass to cargo test
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Run clippy on remote
    Clippy {
        /// All arguments to pass to cargo clippy
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Execute rustup toolchain commands on remote
    Toolchain {
        /// Arguments to pass to rustup toolchain
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Clean remote build directory and local binaries
    Clean,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("warn"));

    let start_time = Instant::now();

    // Get raw command line arguments to preserve "--" separator
    let raw_args: Vec<String> = std::env::args().collect();

    // Parse command line arguments to extract toolchain if specified
    let (toolchain, filtered_args) = parse_cargo_style_args(raw_args.clone());

    // Re-parse with filtered args (without the +toolchain part)
    let cli = Cli::try_parse_from(filtered_args)?;

    // Verify we're in a Rust project
    if !Path::new("Cargo.toml").exists() {
        return Err("Not in a Rust project directory (Cargo.toml not found)".into());
    }

    let offload = CargoOffload::new(&cli, toolchain)?;

    match cli.command {
        Commands::Build { args } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.run_cargo_command("build", &args, &cli.env_vars)?;
            offload.copy_artifacts(&args, None, None)?;
            let elapsed = start_time.elapsed();
            info!(
                "Build completed and artifacts copied successfully (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Run { args } | Commands::RunLocal { args } => {
            let (build_args, run_args) = separate_run_args_from_raw(&args);

            // manually parse args
            let bin = parse_flag(&build_args, "bin")?;
            let example = parse_flag(&build_args, "example")?;

            offload.sync_source()?;
            offload.setup_toolchain()?;

            // Add --bin or --example flag to build args if specified
            let mut final_build_args = build_args;
            if let Some(ref bin_name) = bin {
                final_build_args.push("--bin".to_string());
                final_build_args.push(bin_name.clone());
            } else if let Some(ref example_name) = example {
                final_build_args.push("--example".to_string());
                final_build_args.push(example_name.clone());
            }

            offload.run_cargo_command("build", &final_build_args, &cli.env_vars)?;
            let artifacts =
                offload.copy_artifacts(&final_build_args, bin.as_ref(), example.as_ref())?;

            let artifact_to_run = if let Some(example_name) = &example {
                artifacts
                    .into_iter()
                    .find(|p| {
                        p.parent()
                            .and_then(|parent| parent.file_name())
                            .map(|name| name == "examples")
                            .unwrap_or(false)
                            && p.file_name().unwrap().to_string_lossy() == *example_name
                    })
                    .ok_or_else(|| format!("Example '{}' not found", example_name))?
            } else if let Some(bin_name) = &bin {
                artifacts
                    .into_iter()
                    .find(|p| p.file_name().unwrap().to_string_lossy() == *bin_name)
                    .ok_or_else(|| format!("Binary '{}' not found", bin_name))?
            } else {
                // Find the first binary (not example or library)
                let binaries: Vec<_> = artifacts
                    .into_iter()
                    .filter(|p| {
                        !p.parent()
                            .and_then(|parent| parent.file_name())
                            .map(|name| name == "examples")
                            .unwrap_or(false)
                            && !p.file_name().unwrap().to_string_lossy().starts_with("lib")
                    })
                    .collect();
                debug!(
                    "found binaries: {}",
                    binaries
                        .iter()
                        .map(|p| p.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(", ")
                );

                if binaries.len() == 1 {
                    binaries.into_iter().next().unwrap()
                } else if binaries.is_empty() {
                    return Err("No binaries found to run".into());
                } else {
                    // TODO: determine default binary to run
                    return Err(
                        "Multiple binaries found. Use --bin to specify which one to run".into(),
                    );
                }
            };

            offload.run_binary(&artifact_to_run, &run_args)?;
            let elapsed = start_time.elapsed();
            info!(
                "Run completed successfully (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Test { args } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.run_cargo_command("test", &args, &cli.env_vars)?;
            let elapsed = start_time.elapsed();
            info!(
                "Tests completed successfully (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Clippy { args } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.run_cargo_command("clippy", &args, &cli.env_vars)?;
            let elapsed = start_time.elapsed();
            info!(
                "Clippy completed successfully (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Toolchain { args } => {
            offload.toolchain_remote(&args)?;
        }

        Commands::Clean => {
            offload.clean()?;
            let elapsed = start_time.elapsed();
            info!(
                "Clean completed successfully (took {})",
                format_duration(elapsed)
            );
        }
    }

    Ok(())
}
