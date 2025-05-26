use clap::{Parser, Subcommand};
use log::{debug, info, warn};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
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
struct CargoToml {
    package: Option<Package>,
    workspace: Option<Workspace>,
    bin: Option<Vec<BinaryTarget>>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

#[derive(Deserialize)]
struct Workspace {
    members: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct BinaryTarget {
    name: String,
    path: Option<String>,
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
    is_workspace: bool,
}

impl CargoOffload {
    fn new(cli: &Cli, toolchain: Option<String>) -> Result<Self, Box<dyn std::error::Error>> {
        let cargo_toml = fs::read_to_string("Cargo.toml")?;
        let parsed: CargoToml = toml::from_str(&cargo_toml)?;

        // Check if this is a workspace
        let is_workspace = parsed.workspace.is_some();

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
            is_workspace,
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
        Ok(())
    }

    fn run_cargo_command(
        &self,
        subcommand: &str,
        args: &[String],
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

        let cargo_cmd = format!("cd {} && cargo {}", self.remote_dir, cargo_args.join(" "));

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

    fn copy_binaries(
        &self,
        args: &[String],
        specific_bin: Option<&String>,
    ) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
        let release = args.contains(&"--release".to_string());
        let profile = if release { "release" } else { "debug" };
        let remote_target_dir = format!("{}/target/{}/{}", self.remote_dir, self.target, profile);

        // Create local target directory structure in target/offload/{target_triple}/
        let local_target_dir = format!("target/offload/{}/{}", self.target, profile);
        fs::create_dir_all(&local_target_dir)?;

        // Get list of binaries to copy
        let binaries = if let Some(bin_name) = specific_bin {
            // If specific binary requested, only copy that one
            vec![bin_name.clone()]
        } else {
            // Otherwise get all binaries from the project/workspace
            self.get_binary_names()?
        };

        let mut copied_binaries = Vec::new();

        // Run copy operations in parallel
        let (tx, rx) = mpsc::channel();
        let mut handles = Vec::new();

        for binary_name in binaries {
            let remote_binary = format!("{}/{}", remote_target_dir, binary_name);
            let local_binary = format!("{}/{}", local_target_dir, binary_name);
            let host = self.host.clone();
            let port = self.port;
            let tx = tx.clone();

            let handle = thread::spawn(move || {
                info!("Copying binary: {} -> {}", binary_name, local_binary);

                let mut rsync_cmd = Command::new("rsync");
                rsync_cmd
                    .arg("-a")
                    .arg("--delete")
                    .arg("--compress")
                    .arg("-e")
                    .arg(format!("ssh -p {}", port))
                    .arg(PROGRESS_FLAG)
                    .arg(format!("{}:{}", host, remote_binary))
                    .arg(&local_binary)
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());

                let output = rsync_cmd.output();
                let result = match output {
                    Ok(output) if output.status.success() => {
                        // Make binary executable
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            if let Ok(metadata) = fs::metadata(&local_binary) {
                                let mut perms = metadata.permissions();
                                perms.set_mode(0o755);
                                let _ = fs::set_permissions(&local_binary, perms);
                            }
                        }
                        Ok(PathBuf::from(local_binary))
                    }
                    Ok(output) => Err(format!(
                        "Failed to copy {}: {}",
                        binary_name,
                        String::from_utf8_lossy(&output.stderr)
                    )),
                    Err(e) => Err(format!(
                        "Failed to execute rsync for {}: {}",
                        binary_name, e
                    )),
                };

                tx.send((binary_name, result)).unwrap();
            });

            handles.push(handle);
        }

        // Close the sender to signal completion
        drop(tx);

        // Wait for all threads to complete and collect results
        for handle in handles {
            handle.join().unwrap();
        }

        // Collect results
        for (_binary_name, result) in rx {
            match result {
                Ok(path) => copied_binaries.push(path),
                Err(e) => warn!("{}", e),
            }
        }

        if copied_binaries.is_empty() {
            return Err("No binaries were successfully copied".into());
        }

        info!("Successfully copied {} binaries", copied_binaries.len());
        Ok(copied_binaries)
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

    fn get_binary_names(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let mut binaries = Vec::new();

        if self.is_workspace {
            // For workspaces, we need to discover all binaries across all members
            self.collect_workspace_binaries(&mut binaries)?;
        } else {
            // For regular projects, just check the root Cargo.toml
            self.collect_project_binaries("Cargo.toml", &mut binaries)?;
        }

        Ok(binaries)
    }

    fn collect_workspace_binaries(
        &self,
        binaries: &mut Vec<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cargo_toml = fs::read_to_string("Cargo.toml")?;
        let parsed: CargoToml = toml::from_str(&cargo_toml)?;

        if let Some(workspace) = parsed.workspace {
            if let Some(members) = workspace.members {
                for member in members {
                    // Handle glob patterns in member paths
                    if member.contains('*') {
                        self.collect_glob_binaries(&member, binaries)?;
                    } else {
                        let member_cargo_toml = format!("{}/Cargo.toml", member);
                        if Path::new(&member_cargo_toml).exists() {
                            self.collect_project_binaries(&member_cargo_toml, binaries)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn collect_glob_binaries(
        &self,
        pattern: &str,
        binaries: &mut Vec<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Simple glob handling for patterns like "crates/*"
        let base_path = pattern.trim_end_matches("/*").trim_end_matches('*');

        if let Ok(entries) = fs::read_dir(base_path) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let cargo_toml_path = entry.path().join("Cargo.toml");
                    if cargo_toml_path.exists() {
                        self.collect_project_binaries(
                            &cargo_toml_path.to_string_lossy(),
                            binaries,
                        )?;
                    }
                }
            }
        }

        Ok(())
    }

    fn collect_project_binaries(
        &self,
        cargo_toml_path: &str,
        binaries: &mut Vec<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cargo_toml = fs::read_to_string(cargo_toml_path)?;
        let parsed: CargoToml = toml::from_str(&cargo_toml)?;

        // Determine the base path for this Cargo.toml
        let base_path = Path::new(cargo_toml_path)
            .parent()
            .unwrap_or(Path::new("."));

        // Check if main binary exists (src/main.rs or specified path)
        if let Some(ref package) = parsed.package {
            let main_rs_path = base_path.join("src/main.rs");
            if main_rs_path.exists() && !binaries.contains(&package.name) {
                binaries.push(package.name.clone());
            }
        }

        // Add additional binaries defined in [[bin]] sections
        if let Some(bin_targets) = parsed.bin {
            for bin in bin_targets {
                if !binaries.contains(&bin.name) {
                    let bin_path = if let Some(ref path) = bin.path {
                        base_path.join(path)
                    } else {
                        base_path.join(format!("src/bin/{}.rs", bin.name))
                    };

                    // Only include if the binary source file exists
                    if bin_path.exists() {
                        binaries.push(bin.name);
                    }
                }
            }
        }

        // Check for additional binaries in src/bin/ directory
        let bin_dir = base_path.join("src/bin");
        if bin_dir.exists() {
            if let Ok(entries) = fs::read_dir(&bin_dir) {
                for entry in entries.flatten() {
                    if let Some(file_name) = entry.file_name().to_str() {
                        if file_name.ends_with(".rs") {
                            let bin_name = file_name.trim_end_matches(".rs").to_string();
                            if !binaries.contains(&bin_name) {
                                binaries.push(bin_name);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

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

fn separate_run_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    if let Some(pos) = args.iter().position(|arg| arg == "--") {
        let build_args = args[..pos].to_vec();
        let run_args = args[pos + 1..].to_vec();
        (build_args, run_args)
    } else {
        (args.to_vec(), vec![])
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("warn"));

    let start_time = Instant::now();

    // Parse command line arguments to extract toolchain if specified
    let args: Vec<String> = std::env::args().collect();
    let (toolchain, filtered_args) = parse_cargo_style_args(args);

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
            offload.run_cargo_command("build", &args)?;
            offload.copy_binaries(&args, None)?;
            let elapsed = start_time.elapsed();
            info!(
                "Build completed and binaries copied successfully (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Run { bin, args } => {
            let (build_args, run_args) = separate_run_args(&args);

            offload.sync_source()?;
            offload.setup_toolchain()?;

            // Add --bin flag to build args if specified
            let mut final_build_args = build_args;
            if let Some(ref bin_name) = bin {
                final_build_args.push("--bin".to_string());
                final_build_args.push(bin_name.clone());
            }

            offload.run_cargo_command("build", &final_build_args)?;
            let binaries = offload.copy_binaries(&final_build_args, bin.as_ref())?;

            let binary_to_run = if let Some(bin_name) = &bin {
                binaries
                    .into_iter()
                    .find(|p| p.file_name().unwrap().to_string_lossy() == *bin_name)
                    .ok_or_else(|| format!("Binary '{}' not found", bin_name))?
            } else if binaries.len() == 1 {
                binaries.into_iter().next().unwrap()
            } else {
                return Err(
                    "Multiple binaries found. Use --bin to specify which one to run".into(),
                );
            };

            offload.run_binary(&binary_to_run, &run_args)?;
            let elapsed = start_time.elapsed();
            info!(
                "Run completed successfully (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Test { args } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.run_cargo_command("test", &args)?;
            let elapsed = start_time.elapsed();
            info!(
                "Tests completed successfully (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Clippy { args } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.run_cargo_command("clippy", &args)?;
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
