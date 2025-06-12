use log::{debug, info};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{fs, io};

use crate::util::*;
use crate::Cli;

pub struct CargoOffload {
    host: String,
    port: u16,
    remote_dir: String,
    toolchain: Option<String>,
    target: String,
    copy_all_artifacts: bool,
    progress_flag: String,
}

impl CargoOffload {
    pub fn new(
        cli: &Cli,
        toolchain: Option<String>,
        progress_flag: String,
    ) -> Result<Self, Box<dyn std::error::Error>> {
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

        // Use provided toolchain, detect it from `cargo --version` or use toolchain files
        let final_toolchain = toolchain
            .or_else(|| detect_toolchain_from_cargo().unwrap_or(None))
            .or_else(|| detect_toolchain_from_files().unwrap_or(None));

        Ok(CargoOffload {
            host,
            port,
            remote_dir,
            toolchain: final_toolchain,
            target,
            copy_all_artifacts: cli.copy_all_artifacts,
            progress_flag,
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

    pub fn sync_source(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Syncing source code to remote...");

        // Create remote directory if it doesn't exist
        self.run_ssh_command(&format!("mkdir -p {}", self.remote_dir), false, &[])?;

        // Use rsync to sync source, excluding target directory and other build artifacts
        let mut rsync_cmd = Command::new("rsync");
        rsync_cmd
            .arg("-a")
            .arg("--delete")
            .arg("--compress")
            .arg("-e")
            .arg(format!("ssh -p {}", self.port))
            .arg(&self.progress_flag)
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

    pub fn setup_toolchain(&self) -> Result<(), Box<dyn std::error::Error>> {
        match &self.toolchain {
            Some(toolchain) => {
                info!("Setting up toolchain {} on remote...", toolchain);
                self.run_ssh_command(
                    &format!(
                        "cd {} && rustup toolchain install {}",
                        self.remote_dir, toolchain
                    ),
                    false,
                    &[],
                )?;
            }
            None => {
                // TODO: make sure stable matches?
            }
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

        self.run_ssh_command(&target_install_cmd, false, &[])?;
        Ok(())
    }

    pub fn run_cargo_command(
        &self,
        subcommand: &str,
        args: &[String],
        env_vars: &[String],
        forward_ports: &[String],
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

        self.run_ssh_command(&cargo_cmd, true, forward_ports)?;
        debug!("Cargo {} completed successfully on remote", subcommand);

        Ok(())
    }

    pub fn toolchain_remote(&self, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
        debug!("Running rustup toolchain command on remote...");

        let toolchain_cmd = format!("rustup toolchain {}", args.join(" "));
        self.run_ssh_command(&toolchain_cmd, true, &[])?;
        debug!("Toolchain command completed successfully on remote");

        Ok(())
    }

    pub fn copy_artifacts(
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
            .arg(&self.progress_flag)
            .arg("--exclude=.cargo-lock")
            .arg("--exclude=*.d"); // TODO: can we improve this by not excluding?

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

    pub fn clean(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Cleaning remote build directory...");

        // Clean remote directory
        self.run_ssh_command(&format!("rm -rf {}", self.remote_dir), false, &[])?;

        // Clean local offload target directory
        let local_offload_dir = "target/offload";
        if Path::new(local_offload_dir).exists() {
            info!("Cleaning local offload directory...");
            fs::remove_dir_all(local_offload_dir)?;
        }

        info!("Clean completed successfully");
        Ok(())
    }

    pub fn run_binary(
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

    fn run_ssh_command(
        &self,
        command: &str,
        print_output: bool,
        forward_ports: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut ssh_cmd = Command::new("ssh");

        // Force pseudo-terminal allocation for interactive programs
        ssh_cmd.arg("-t");

        if !forward_ports.is_empty() {
            let mut ssh_forward_args = Vec::new();
            for port_spec in forward_ports {
                // Parse format: local_port:remote_port or just port (assumes same port on both sides)
                let parts: Vec<&str> = port_spec.split(':').collect();
                match parts.len() {
                    1 => {
                        // Same port on both sides
                        ssh_forward_args.push("-L".to_string());
                        ssh_forward_args.push(format!("{}:localhost:{}", parts[0], parts[0]));
                    }
                    2 => {
                        // Different ports: local:remote
                        ssh_forward_args.push("-L".to_string());
                        ssh_forward_args.push(format!("{}:localhost:{}", parts[0], parts[1]));
                    }
                    _ => {
                        return Err(format!(
                            "Invalid port forwarding specification: {}",
                            port_spec
                        )
                        .into());
                    }
                }
            }

            // Disable strict host key check
            // ssh_cmd.arg("-o").arg("StrictHostKeyChecking=no");

            // Add port forwarding arguments
            info!("Port forwarding: {}", forward_ports.join(", "));
            for arg in ssh_forward_args {
                ssh_cmd.arg(&arg);
            }
        }

        ssh_cmd
            .arg("-p")
            .arg(self.port.to_string())
            .arg(&self.host)
            .arg(command);

        if print_output {
            ssh_cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
            let status = ssh_cmd.status()?;
            if !status.success() {
                return Err(format!("SSH command failed: {}", command).into());
            }
        } else {
            let output = ssh_cmd.output()?;
            let status = output.status;
            if !status.success() {
                io::stdout().write_all(&output.stdout)?;
                io::stderr().write_all(&output.stderr)?;
                return Err(format!("SSH command failed: {}", command).into());
            }
        }

        Ok(())
    }
}
