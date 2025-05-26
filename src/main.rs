use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const PROGRESS_FLAG: &str = "--info=progress2";

#[derive(Parser)]
#[command(name = "offload")]
#[command(about = "A CLI tool for remote Rust compilation")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// SSH host (user@hostname or just hostname)
    #[arg(short, long)]
    host: Option<String>,

    /// SSH port (defaults to 22, can also be specified in CARGO_OFFLOAD_HOST)
    #[arg(short, long)]
    port: Option<u16>,

    /// Target triple (defaults to x86_64-unknown-linux-gnu)
    #[arg(long)]
    target: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Build the project on remote and copy binaries back
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,

        /// Build all features
        #[arg(long)]
        all_features: bool,

        /// Build with specific features
        #[arg(long)]
        features: Option<String>,

        /// Additional cargo build arguments
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Build on remote, copy binaries, and run locally
    Run {
        /// Build in release mode
        #[arg(long)]
        release: bool,

        /// Build all features
        #[arg(long)]
        all_features: bool,

        /// Build with specific features
        #[arg(long)]
        features: Option<String>,

        /// Binary name to run (if multiple binaries exist)
        #[arg(long)]
        bin: Option<String>,

        /// Arguments to pass to the binary
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Run tests on remote
    Test {
        /// Run tests in release mode
        #[arg(long)]
        release: bool,

        /// Test all features
        #[arg(long)]
        all_features: bool,

        /// Test with specific features
        #[arg(long)]
        features: Option<String>,

        /// Test only the specified test target
        #[arg(long)]
        test: Option<String>,

        /// Test only the library
        #[arg(long)]
        lib: bool,

        /// Test only the specified binary
        #[arg(long)]
        bin: Option<String>,

        /// Test all binaries
        #[arg(long)]
        bins: bool,

        /// Test all examples
        #[arg(long)]
        examples: bool,

        /// Test all documentation tests
        #[arg(long)]
        doc: bool,

        /// Run ignored tests
        #[arg(long)]
        ignored: bool,

        /// Include ignored tests when running tests
        #[arg(long)]
        include_ignored: bool,

        /// Test only this package's library unit tests
        #[arg(long)]
        no_run: bool,

        /// Do not capture test output
        #[arg(long)]
        nocapture: bool,

        /// Number of parallel jobs, defaults to # of CPUs
        #[arg(short = 'j', long)]
        jobs: Option<u32>,

        /// Additional cargo test arguments
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Run clippy on remote
    Clippy {
        /// Run clippy in release mode
        #[arg(long)]
        release: bool,

        /// Check all features
        #[arg(long)]
        all_features: bool,

        /// Check with specific features
        #[arg(long)]
        features: Option<String>,

        /// Additional cargo clippy arguments
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Execute rustup toolchain commands on remote
    Toolchain {
        /// Arguments to pass to rustup toolchain
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
    local_folder_name: String,
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
            local_folder_name,
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
                println!("Detected toolchain from rust-toolchain.toml: {}", toolchain);
                return Ok(Some(toolchain));
            }
        }

        // Try rust-toolchain file (plain text format)
        if Path::new("rust-toolchain").exists() {
            let content = fs::read_to_string("rust-toolchain")?;
            let toolchain = content.trim().to_string();
            if !toolchain.is_empty() {
                println!("Detected toolchain from rust-toolchain: {}", toolchain);
                return Ok(Some(toolchain));
            }
        }

        Ok(None)
    }

    fn sync_source(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("Syncing source code to remote...");

        // Create remote directory if it doesn't exist
        self.run_ssh_command(&format!("mkdir -p {}", self.remote_dir))?;

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
            println!("Setting up toolchain {} on remote...", toolchain);
            self.run_ssh_command(&format!(
                "cd {} && rustup toolchain install {}",
                self.remote_dir, toolchain
            ))?;
        }
        Ok(())
    }

    fn build_remote(
        &self,
        release: bool,
        all_features: bool,
        features: Option<&String>,
        extra_args: &[String],
        bin: Option<&String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!("Building on remote...");

        let mut build_args = Vec::new();

        // Add toolchain prefix
        if let Some(toolchain) = &self.toolchain {
            build_args.push(format!("+{}", toolchain));
        }

        build_args.push("build".to_string());

        // Add release flag
        if release {
            build_args.push("--release".to_string());
        }

        // Add target
        build_args.push("--target".to_string());
        build_args.push(self.target.clone());

        // Add features
        if all_features {
            build_args.push("--all-features".to_string());
        } else if let Some(features_str) = features {
            build_args.push("--features".to_string());
            build_args.push(features_str.clone());
        }

        // Add bin flag if specified
        if let Some(bin_name) = bin {
            build_args.push("--bin".to_string());
            build_args.push(bin_name.clone());
        }

        // Add extra arguments
        build_args.extend(extra_args.iter().cloned());

        let build_cmd = format!("cd {} && cargo {}", self.remote_dir, build_args.join(" "));

        self.run_ssh_command(&build_cmd)?;
        println!("Build completed successfully on remote");

        Ok(())
    }

    fn clippy_remote(
        &self,
        release: bool,
        all_features: bool,
        features: Option<&String>,
        extra_args: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!("Running clippy on remote...");

        let mut clippy_args = Vec::new();

        // Add toolchain prefix
        if let Some(toolchain) = &self.toolchain {
            clippy_args.push(format!("+{}", toolchain));
        }

        clippy_args.push("clippy".to_string());

        // Add release flag
        if release {
            clippy_args.push("--release".to_string());
        }

        // Add target
        clippy_args.push("--target".to_string());
        clippy_args.push(self.target.clone());

        // Add features
        if all_features {
            clippy_args.push("--all-features".to_string());
        } else if let Some(features_str) = features {
            clippy_args.push("--features".to_string());
            clippy_args.push(features_str.clone());
        }

        // Add extra arguments
        clippy_args.extend(extra_args.iter().cloned());

        let clippy_cmd = format!("cd {} && cargo {}", self.remote_dir, clippy_args.join(" "));

        self.run_ssh_command(&clippy_cmd)?;
        println!("Clippy completed successfully on remote");

        Ok(())
    }

    fn test_remote(
        &self,
        release: bool,
        all_features: bool,
        features: Option<&String>,
        test: Option<&String>,
        lib: bool,
        bin: Option<&String>,
        bins: bool,
        examples: bool,
        doc: bool,
        ignored: bool,
        include_ignored: bool,
        no_run: bool,
        nocapture: bool,
        jobs: Option<u32>,
        extra_args: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!("Running tests on remote...");

        let mut test_args = Vec::new();

        // Add toolchain prefix
        if let Some(toolchain) = &self.toolchain {
            test_args.push(format!("+{}", toolchain));
        }

        test_args.push("test".to_string());

        // Add release flag
        if release {
            test_args.push("--release".to_string());
        }

        // Add target
        test_args.push("--target".to_string());
        test_args.push(self.target.clone());

        // Add features
        if all_features {
            test_args.push("--all-features".to_string());
        } else if let Some(features_str) = features {
            test_args.push("--features".to_string());
            test_args.push(features_str.clone());
        }

        // Add test target flags
        if let Some(test_name) = test {
            test_args.push("--test".to_string());
            test_args.push(test_name.clone());
        }

        if lib {
            test_args.push("--lib".to_string());
        }

        if let Some(bin_name) = bin {
            test_args.push("--bin".to_string());
            test_args.push(bin_name.clone());
        }

        if bins {
            test_args.push("--bins".to_string());
        }

        if examples {
            test_args.push("--examples".to_string());
        }

        if doc {
            test_args.push("--doc".to_string());
        }

        // Add test execution flags
        if ignored {
            test_args.push("--ignored".to_string());
        }

        if include_ignored {
            test_args.push("--include-ignored".to_string());
        }

        if no_run {
            test_args.push("--no-run".to_string());
        }

        if nocapture {
            test_args.push("--nocapture".to_string());
        }

        if let Some(job_count) = jobs {
            test_args.push("--jobs".to_string());
            test_args.push(job_count.to_string());
        }

        // Add extra arguments
        test_args.extend(extra_args.iter().cloned());

        let test_cmd = format!("cd {} && cargo {}", self.remote_dir, test_args.join(" "));

        self.run_ssh_command(&test_cmd)?;
        println!("Tests completed successfully on remote");

        Ok(())
    }

    fn toolchain_remote(&self, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
        println!("Running rustup toolchain command on remote...");

        let toolchain_cmd = format!("rustup toolchain {}", args.join(" "));
        self.run_ssh_command(&toolchain_cmd)?;
        println!("Toolchain command completed successfully on remote");

        Ok(())
    }

    fn copy_binaries(
        &self,
        release: bool,
        specific_bin: Option<&String>,
    ) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
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

        for binary_name in binaries {
            let remote_binary = format!("{}/{}", remote_target_dir, binary_name);
            let local_binary = format!("{}/{}", local_target_dir, binary_name);

            println!("Copying binary: {} -> {}", binary_name, local_binary);

            let mut rsync_cmd = Command::new("rsync");
            rsync_cmd
                .arg("-a")
                .arg("--delete")
                .arg("--compress")
                .arg("-e")
                .arg(format!("ssh -p {}", self.port))
                .arg(PROGRESS_FLAG)
                .arg(format!("{}:{}", self.host, remote_binary))
                .arg(&local_binary)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());

            let output = rsync_cmd.output()?;
            if !output.status.success() {
                eprintln!(
                    "Warning: Failed to copy {}: {}",
                    binary_name,
                    String::from_utf8_lossy(&output.stderr)
                );
                continue;
            }

            // Make binary executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&local_binary)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&local_binary, perms)?;
            }

            copied_binaries.push(PathBuf::from(local_binary));
        }

        if copied_binaries.is_empty() {
            return Err("No binaries were successfully copied".into());
        }

        println!("Successfully copied {} binaries", copied_binaries.len());
        Ok(copied_binaries)
    }

    fn clean(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("Cleaning remote build directory...");

        // Clean remote directory
        self.run_ssh_command(&format!("rm -rf {}", self.remote_dir))?;

        // Clean local offload target directory
        let local_offload_dir = "target/offload";
        if Path::new(local_offload_dir).exists() {
            println!("Cleaning local offload directory...");
            fs::remove_dir_all(local_offload_dir)?;
        }

        println!("Clean completed successfully!");
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

        // Add main binary (same name as package)
        if let Some(ref package) = parsed.package {
            if !binaries.contains(&package.name) {
                binaries.push(package.name.clone());
            }
        }

        // Add additional binaries defined in [[bin]] sections
        if let Some(bin_targets) = parsed.bin {
            for bin in bin_targets {
                if !binaries.contains(&bin.name) {
                    binaries.push(bin.name);
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
        println!("Running: {} {}", binary_path.display(), args.join(" "));

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
}

fn parse_cargo_style_args(args: Vec<String>) -> (Option<String>, Vec<String>) {
    if let Some(first_arg) = args.first() {
        if first_arg.starts_with('+') {
            let toolchain = first_arg[1..].to_string();
            let remaining_args = args.into_iter().skip(1).collect();
            return (Some(toolchain), remaining_args);
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        Commands::Build {
            release,
            all_features,
            features,
            args,
        } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.build_remote(release, all_features, features.as_ref(), &args, None)?;
            offload.copy_binaries(release, None)?;
            let elapsed = start_time.elapsed();
            println!(
                "Build completed and binaries copied successfully! (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Run {
            release,
            all_features,
            features,
            bin,
            args,
        } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.build_remote(release, all_features, features.as_ref(), &[], bin.as_ref())?;
            let binaries = offload.copy_binaries(release, bin.as_ref())?;

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

            offload.run_binary(&binary_to_run, &args)?;
            let elapsed = start_time.elapsed();
            println!(
                "Run completed successfully! (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Test {
            release,
            all_features,
            features,
            test,
            lib,
            bin,
            bins,
            examples,
            doc,
            ignored,
            include_ignored,
            no_run,
            nocapture,
            jobs,
            args,
        } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.test_remote(
                release,
                all_features,
                features.as_ref(),
                test.as_ref(),
                lib,
                bin.as_ref(),
                bins,
                examples,
                doc,
                ignored,
                include_ignored,
                no_run,
                nocapture,
                jobs,
                &args,
            )?;
            let elapsed = start_time.elapsed();
            println!(
                "Tests completed successfully! (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Clippy {
            release,
            all_features,
            features,
            args,
        } => {
            offload.sync_source()?;
            offload.setup_toolchain()?;
            offload.clippy_remote(release, all_features, features.as_ref(), &args)?;
            let elapsed = start_time.elapsed();
            println!(
                "Clippy completed successfully! (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Toolchain { args } => {
            offload.toolchain_remote(&args)?;
            let elapsed = start_time.elapsed();
            println!(
                "Toolchain command completed successfully! (took {})",
                format_duration(elapsed)
            );
        }

        Commands::Clean => {
            offload.clean()?;
            let elapsed = start_time.elapsed();
            println!(
                "Clean completed successfully! (took {})",
                format_duration(elapsed)
            );
        }
    }

    Ok(())
}
