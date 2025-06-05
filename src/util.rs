use std::{fs, path::Path};

use anyhow::{bail, Context, Result};
use log::debug;
use serde::Deserialize;

pub fn parse_cargo_style_args(raw_args: Vec<String>) -> (Option<String>, Vec<String>) {
    if let Some(pos) = raw_args.iter().position(|arg| arg.starts_with("+")) {
        let toolchain = raw_args[pos].trim_start_matches('+');
        let mut first = raw_args[..pos].to_vec();
        let last = &raw_args[pos + 1..];
        first.extend_from_slice(last);
        (Some(toolchain.to_string()), first)
    } else {
        (None, raw_args)
    }
}

pub fn separate_run_args_from_raw(raw_args: &[String]) -> (Vec<String>, Vec<String>) {
    if let Some(pos) = raw_args.iter().position(|arg| arg == "--") {
        let build_args = raw_args[..pos].to_vec();
        let run_args = raw_args[pos + 1..].to_vec();
        (build_args, run_args)
    } else {
        (raw_args.to_vec(), vec![])
    }
}

pub fn parse_flag(args: &[String], arg: &str) -> Result<Option<String>> {
    if args.is_empty() {
        return Ok(None);
    }

    for i in 0..(args.len() - 1) {
        if let Some(a) = args[i].strip_prefix("--") {
            let s = a.split("=").collect::<Vec<_>>();
            if s.len() == 2 {
                // found `--key=value` arg
                if s[0] == arg {
                    return Ok(Some(s[1].to_string()));
                }
            } else if a == arg {
                // found regular `--key value` arg
                let v = args[i + 1].clone();
                if v.starts_with("-") {
                    bail!("Invalid argument `--{a} {v}`");
                }

                return Ok(Some(v));
            }
        }
    }

    Ok(None)
}

pub fn detect_toolchain_from_cargo() -> Result<Option<String>> {
    let output = std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .context("Executing `cargo --version` failed")?;

    if output.status.success() {
        let stdout =
            String::from_utf8(output.stdout).context("Invalid `cargo --version` output")?;
        let stdout = stdout.trim();
        let splits = stdout.split(" ").collect::<Vec<_>>();

        // cargo 1.87.0 (99624be96 2025-05-06)
        if splits.len() >= 2 && splits[0] == "cargo" {
            return Ok(Some(splits[1].to_string()));
        }
    }

    Ok(None)
}

pub fn detect_toolchain_from_files() -> Result<Option<String>, Box<dyn std::error::Error>> {
    #[derive(Deserialize)]
    struct RustToolchainToml {
        pub toolchain: Option<ToolchainConfig>,
    }

    #[derive(Deserialize)]
    struct ToolchainConfig {
        pub channel: Option<String>,
    }

    // Try rust-toolchain.toml first
    if Path::new("rust-toolchain.toml").exists() {
        let content =
            fs::read_to_string("rust-toolchain.toml").context("Cannot open rust-toolchain.toml")?;
        let parsed: RustToolchainToml =
            toml::from_str(&content).context("Cannot parse rust-toolchain.toml")?;
        if let Some(toolchain) = parsed.toolchain.and_then(|t| t.channel) {
            debug!("Detected toolchain from rust-toolchain.toml: {}", toolchain);
            return Ok(Some(toolchain));
        }
    }

    // Try rust-toolchain file (plain text format)
    if Path::new("rust-toolchain").exists() {
        let content = fs::read_to_string("rust-toolchain").context("Cannot open rust-toolchain")?;
        let toolchain = content.trim().to_string();
        if !toolchain.is_empty() {
            debug!("Detected toolchain from rust-toolchain: {}", toolchain);
            return Ok(Some(toolchain));
        }
    }

    Ok(None)
}

pub fn format_duration(duration: std::time::Duration) -> String {
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
