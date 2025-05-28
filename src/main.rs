use clap::{Parser, Subcommand};
use log::{debug, info};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use std::{fs, io};

const PROGRESS_FLAG: &str = "--info=progress2";

#[derive(Parser)]
#[command(name = "offload")]
#[command(about = "A CLI tool for remote Rust compilation")]
#[command(disable_help_subcommand = true)]
struct Cli {
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
enum Commands {
    /// Build the project on remote and copy binaries back
    Build {
        /// All arguments to pass to cargo build
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Build on remote, copy binaries, and run locally
    Run {
        /// Binary name to run (if multiple binaries exist)
        #[arg(long)]
        bin: Option<String>,

        /// Example name to run
        #[arg(long)]
        example: Option<String>,

        /// All arguments to pass to cargo build and the binary
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Run tests on remote
    Test {
        /// All arguments to pass to cargo test
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Run clippy on remote
    Clippy {
        /// All arguments to pass to cargo clippy
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Execute rustup toolchain commands on remote
    Toolchain {
        /// Arguments to pass to rustup toolchain
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Clean remote build directory and local binaries
    Clean,
}

#[derive(Deserialize)]
struct RustToolchainToml {
    toolchain: Option<ToolchainConfig>,
}

#[derive(Deserialize)]
struct ToolchainConfig {
    channel: Option<String>,
}

struct CargoOffload {
    host: String,
    port: u16,
    remote_dir: String,
    toolchain: Option<String>,
    target: String,
    copy_all_artifacts: bool,
}

impl CargoOffload {
    fn new(cli: &Cli, toolchain: Option<String>) -> Result<Self, Box<dyn std::error::Error>> {
        // Parse host and port from environment variable or CLI args
        let (host, port) = Self::parse_host_and_port(cli)?;
        info!("Executing command on {}:{}", host, port);

        // Get current folder name
        let current_dir = std::env::current_dir()?;
        let local_folder_name = current_dir
            .file_name()
            .ok_or("Cannot determine current folder name")?
            .to_string_lossy()
            .to_string();

        let remote_dir = format!("/tmp/cargo-offload/{}", local_folder_name);

        let target = cli
            .target
            .clone()
            .unwrap_or_else(|| "x86_64-unknown-linux-gnu".to_string());

        // Use provided toolchain or detect from files
        let final_toolchain = toolchain.or_else(|| Self::detect_toolchain().unwrap_or(None));

        Ok(CargoOffload {
            host,
            port,
            remote_dir,
            toolchain: final_toolchain,
            target,
            copy_all_artifacts: cli.copy_all_artifacts,
        })
    }

    fn parse_host_and_port(cli: &Cli) -> Result<(String, u16), Box<dyn std::error::Error>> {
        let host_str = cli
            .host
            .clone()
            .or_else(|| std::env::var("CARGO_OFFLOAD_HOST").ok())
            .ok_or("Host must be specified via --host or CARGO_OFFLOAD_HOST env var")?;

        // Parse format: user@host:port or host:port or just host
        if let Some(colon_pos) = host_str.rfind(':') {
            let (host_part, port_part) = host_str.split_at(colon_pos);
            let port_str = &port_part[1..]; // Remove the ':'

            if let Ok(port) = port_str.parse::<u16>() {
                let final_port = cli.port.unwrap_or(port);
                return Ok((host_part.to_string(), final_port));
            }
        }

        // No port in host string, use CLI arg or default
        let port = cli.port.unwrap_or(22);
        Ok((host_str, port))
    }

    fn detect_toolchain() -> Result<Option<String>, Box<dyn std::error::Error>> {
        // Try rust-toolchain.toml first
        if Path::new("rust-toolchain.toml").exists() {
            let content = fs::read_to_string("rust-toolchain.toml")?;
            let parsed: RustToolchainToml = toml::from_str(&content)?;
            if let Some(toolchain) = parsed.toolchain.and_then(|t| t.channel) {
                debug!("Detected toolchain from rust-toolchain.toml: {}", toolchain);
                return Ok(Some(toolchain));
            }
        }

        // Try rust-toolchain file (plain text format)
        if Path::new("rust-toolchain").exists() {
            let content = fs::read_to_string("rust-toolchain")?;
            let toolchain = content.trim().to_string();
            if !toolchain.is_empty() {
                debug!("Detected toolchain from rust-toolchain: {}", toolchain);
                return Ok(Some(toolchain));
            }
        }

        Ok(None)
    }

    fn sync_source(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Syncing source code to remote...");

        // Create remote directory if it doesn't exist
        self.run_ssh_command_silent(&format!("mkdir -p {}", self.remote_dir))?;

        // Use rsync to sync source, excluding target directory and other build artifacts
        let mut rsync_cmd = Command::new("rsync");
        rsync_cmd
            .arg("-a")
            .arg("--delete")
            .arg("--compress")
            .arg("-e")
            .arg(format!("ssh -p {}", self.port))
            .arg(PROGRESS_FLAG)
            .arg("--exclude=target/")
            .arg("--exclude=.git/")
            .arg("--exclude=*.swp")
            .arg("--exclude=*.tmp")
            .arg("--exclude=.cargo/")
            .arg(".")
            .arg(format!("{}:{}/", self.host, self.remote_dir))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let output = rsync_cmd.output()?;
        if !output.status.success() {
            return Err(
                format!("rsync failed: {}", String::from_utf8_lossy(&output.stderr)).into(),
            );
        }

        Ok(())
    }

    fn setup_toolchain(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(toolchain) = &self.toolchain {
            info!("Setting up toolchain {} on remote...", toolchain);
            self.run_ssh_command_silent(&format!(
                "cd {} && rustup toolchain install {}",
                self.remote_dir, toolchain
            ))?;
        }

        // Always ensure the target is installed
        info!("Ensuring target {} is installed on remote...", self.target);
        let target_install_cmd = if let Some(toolchain) = &self.toolchain {
            format!(
                "cd {} && rustup target add {} --toolchain {}",
                self.remote_dir, self.target, toolchain
            )
        } else {
            format!(
                "cd {} && rustup target add {}",
                self.remote_dir, self.target
            )
        };

        self.run_ssh_command_silent(&target_install_cmd)?;
        Ok(())
    }

    fn run_cargo_command(
        &self,
        subcommand: &str,
        args: &[String],
        env_vars: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Running cargo {} on remote...", subcommand);

        let mut cargo_args = Vec::new();

        // Add toolchain prefix
        if let Some(toolchain) = &self.toolchain {
            cargo_args.push(format!("+{}", toolchain));
        }

        cargo_args.push(subcommand.to_string());

        // Parse args to insert target if needed and not already present
        let mut has_target = false;
        let mut final_args = Vec::new();

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--target" {
                has_target = true;
                final_args.push(arg.clone());
                if i + 1 < args.len() {
                    i += 1;
                    final_args.push(args[i].clone());
                }
            } else {
                final_args.push(arg.clone());
            }
            i += 1;
        }

        // Add default target if not specified
        if !has_target {
            cargo_args.push("--target".to_string());
            cargo_args.push(self.target.clone());
        }

        // Add user arguments
        cargo_args.extend(final_args);

        // Construct the command with environment variables
        let env_vars_str = if !env_vars.is_empty() {
            // Properly quote environment variables to handle spaces in values
            let quoted_env_vars: Vec<String> = env_vars
                .iter()
                .map(|var| {
                    // Split at the first equals sign
                    if let Some(pos) = var.find('=') {
                        let (name, value) = var.split_at(pos + 1);
                        // Quote the value part if it contains spaces or special characters
                        if value.contains(' ')
                            || value.contains('"')
                            || value.contains('\'')
                            || value.contains('$')
                            || value.contains('&')
                            || value.contains('|')
                        {
                            // Use single quotes for values with spaces, escaping any single quotes in the value
                            let escaped_value = value.replace('\'', "'\\''");
                            format!("{}'{}'", name, escaped_value)
                        } else {
                            var.clone()
                        }
                    } else {
                        // If there's no equals sign, just use as is
                        var.clone()
                    }
                })
                .collect();
            format!("{} ", quoted_env_vars.join(" "))
        } else {
            String::new()
        };

        let cargo_cmd = format!(
            "cd {} && {}cargo {}",
            self.remote_dir,
            env_vars_str,
            cargo_args.join(" ")
        );

        self.run_ssh_command(&cargo_cmd)?;
        debug!("Cargo {} completed successfully on remote", subcommand);

        Ok(())
    }

    fn toolchain_remote(&self, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
        debug!("Running rustup toolchain command on remote...");

        let toolchain_cmd = format!("rustup toolchain {}", args.join(" "));
        self.run_ssh_command(&toolchain_cmd)?;
        debug!("Toolchain command completed successfully on remote");

        Ok(())
    }

    fn copy_artifacts(
        &self,
        args: &[String],
        specific_bin: Option<&String>,
        specific_example: Option<&String>,
    ) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
        let release = args.contains(&"--release".to_string());
        let profile = if release { "release" } else { "debug" };
        let remote_target_dir = format!("{}/target/{}", self.remote_dir, self.target);
        let remote_profile_dir = format!("{}/{}", remote_target_dir, profile);

        // Create local target directory structure in target/offload/{target_triple}/
        let local_target_dir = format!("target/offload/{}", self.target);
        let local_profile_dir = format!("{}/{}", local_target_dir, profile);
        fs::create_dir_all(&local_profile_dir)?;

        info!("Copying artifacts from remote target directory...");

        // Use a single rsync call to copy the entire target directory
        let mut rsync_cmd = Command::new("rsync");
        rsync_cmd
            .arg("-a")
            .arg("--delete")
            .arg("--compress")
            .arg("-e")
            .arg(format!("ssh -p {}", self.port))
            .arg(PROGRESS_FLAG);

        // Add exclusions for large build artifacts unless --copy-all-artifacts is specified
        if !self.copy_all_artifacts {
            rsync_cmd
                .arg("--exclude=build/")
                .arg("--exclude=deps/")
                .arg("--exclude=incremental/");
        }

        // Set source and destination
        rsync_cmd
            .arg(format!("{}:{}/", self.host, remote_profile_dir))
            .arg(format!("{}/", local_profile_dir))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let output = rsync_cmd.output()?;
        if !output.status.success() {
            return Err(format!(
                "Failed to copy artifacts: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        // Make binaries and examples executable on Unix systems
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // Find and make executable all binary files in the target directory
            let make_executable = |path: &Path| {
                if let Ok(metadata) = fs::metadata(path) {
                    // Only make executable if it's a file and not a directory
                    if metadata.is_file() {
                        let mut perms = metadata.permissions();
                        perms.set_mode(0o755);
                        let _ = fs::set_permissions(path, perms);
                    }
                }
            };

            // Make binaries in root directory executable
            if let Ok(entries) = fs::read_dir(&local_profile_dir) {
                for entry in entries.flatten() {
                    if let Ok(file_type) = entry.file_type() {
                        if file_type.is_file() {
                            make_executable(&entry.path());
                        }
                    }
                }
            }

            // Make examples executable
            let examples_dir = format!("{}/examples", local_profile_dir);
            if Path::new(&examples_dir).exists() {
                if let Ok(entries) = fs::read_dir(&examples_dir) {
                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_file() {
                                make_executable(&entry.path());
                            }
                        }
                    }
                }
            }
        }

        // Determine which binary to return for the run command
        let mut result_paths = Vec::new();

        if let Some(bin_name) = specific_bin {
            // If a specific binary was requested
            let bin_path = PathBuf::from(format!("{}/{}", local_profile_dir, bin_name));
            if bin_path.exists() {
                result_paths.push(bin_path);
            } else {
                return Err(format!("Binary '{}' not found after copy", bin_name).into());
            }
        } else if let Some(example_name) = specific_example {
            // If a specific example was requested
            let example_path =
                PathBuf::from(format!("{}/examples/{}", local_profile_dir, example_name));
            if example_path.exists() {
                result_paths.push(example_path);
            } else {
                return Err(format!("Example '{}' not found after copy", example_name).into());
            }
        } else {
            // For general build, just return success without specific paths
            // Find all executables in the root directory (not in subdirectories)
            if let Ok(entries) = fs::read_dir(&local_profile_dir) {
                for entry in entries.flatten() {
                    if let Ok(file_type) = entry.file_type() {
                        if file_type.is_file() {
                            let path = entry.path();
                            // Skip files that start with "lib" as they're likely libraries
                            if let Some(file_name) = path.file_name() {
                                let name = file_name.to_string_lossy();
                                if !name.starts_with("lib") {
                                    result_paths.push(path);
                                }
                            }
                        }
                    }
                }
            }

            // Also check examples directory
            let examples_dir = format!("{}/examples", local_profile_dir);
            if Path::new(&examples_dir).exists() {
                if let Ok(entries) = fs::read_dir(&examples_dir) {
                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_file() {
                                result_paths.push(entry.path());
                            }
                        }
                    }
                }
            }
        }

        info!("Successfully copied artifacts from remote target directory");
        Ok(result_paths)
    }

    fn clean(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Cleaning remote build directory...");

        // Clean remote directory
        self.run_ssh_command_silent(&format!("rm -rf {}", self.remote_dir))?;

        // Clean local offload target directory
        let local_offload_dir = "target/offload";
        if Path::new(local_offload_dir).exists() {
            info!("Cleaning local offload directory...");
            fs::remove_dir_all(local_offload_dir)?;
        }

        info!("Clean completed successfully");
        Ok(())
    }

    // Removed artifact discovery methods as they're no longer needed

    fn run_binary(
        &self,
        binary_path: &Path,
        args: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Running: {} {}", binary_path.display(), args.join(" "));

        let mut cmd = Command::new(binary_path);
        cmd.args(args);

        let status = cmd.status()?;

        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }

        Ok(())
    }

    fn run_ssh_command(&self, command: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut ssh_cmd = Command::new("ssh");
        ssh_cmd
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-t")
            .arg(&self.host)
            .arg(command)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let status = ssh_cmd.status()?;
        if !status.success() {
            return Err(format!("SSH command failed: {}", command).into());
        }

        Ok(())
    }

    fn run_ssh_command_silent(&self, command: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut ssh_cmd = Command::new("ssh");
        ssh_cmd
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-t")
            .arg(&self.host)
            .arg(command);

        let output = ssh_cmd.output()?;
        let status = output.status;
        if !status.success() {
            io::stdout().write_all(&output.stdout)?;
            io::stderr().write_all(&output.stderr)?;
            return Err(format!("SSH command failed: {}", command).into());
        }

        Ok(())
    }
}

fn parse_cargo_style_args(args: Vec<String>) -> (Option<String>, Vec<String>) {
    if let Some(first_arg) = args.first() {
        if let Some(toolchain) = first_arg.clone().strip_prefix("+") {
            let remaining_args = args.into_iter().skip(1).collect();
            return (Some(toolchain.to_string()), remaining_args);
        }
    }
    (None, args)
}

fn format_duration(duration: std::time::Duration) -> String {
    let total_secs = duration.as_secs();
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    let millis = duration.subsec_millis();

    if minutes > 0 {
        format!("{}m {}.{:03}s", minutes, seconds, millis)
    } else {
        format!("{}.{:03}s", seconds, millis)
    }
}

fn separate_run_args_from_raw(raw_args: &[String]) -> (Vec<String>, Vec<String>) {
    if let Some(pos) = raw_args.iter().position(|arg| arg == "--") {
        let build_args = raw_args[..pos].to_vec();
        let run_args = raw_args[pos + 1..].to_vec();
        (build_args, run_args)
    } else {
        (raw_args.to_vec(), vec![])
    }
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

        Commands::Run {
            bin,
            example,
            args: _,
        } => {
            // For run command, we need to parse raw args to handle "--" separator properly
            // Skip the program name and "run" command, then extract relevant args
            let mut run_args_start = 1; // Skip program name

            // Skip past global args and the "run" subcommand
            let mut i = 1;
            while i < raw_args.len() {
                let arg = &raw_args[i];
                if arg == "run" {
                    run_args_start = i + 1;
                    break;
                }
                // Skip global args with values
                if arg == "--host" || arg == "--port" || arg == "--target" {
                    i += 1; // Skip the value too
                } else if arg.starts_with("--host=")
                    || arg.starts_with("--port=")
                    || arg.starts_with("--target=")
                {
                    // Single arg with = format, no need to skip extra
                }
                i += 1;
            }

            // Handle --bin and --example arguments if present
            while run_args_start < raw_args.len() {
                let arg = &raw_args[run_args_start];
                if arg == "--bin" || arg == "--example" {
                    run_args_start += 2; // Skip flag and its value
                } else {
                    break;
                }
            }

            let run_raw_args = &raw_args[run_args_start..];
            let (build_args, run_args) = separate_run_args_from_raw(run_raw_args);

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

                if binaries.len() == 1 {
                    binaries.into_iter().next().unwrap()
                } else if binaries.is_empty() {
                    return Err("No binaries found to run".into());
                } else {
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
