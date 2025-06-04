use std::{fs, path::Path};

use log::debug;
use serde::Deserialize;

pub fn parse_cargo_style_args(args: Vec<String>) -> (Option<String>, Vec<String>) {
    if let Some(first_arg) = args.first() {
        if let Some(toolchain) = first_arg.clone().strip_prefix("+") {
            let remaining_args = args.into_iter().skip(1).collect();
            return (Some(toolchain.to_string()), remaining_args);
        }
    }
    (None, args)
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

// TODO: should support --long=value syntax as well
pub fn parse_flag(args: &[String], arg: &str) -> Option<String> {
    for i in 0..(args.len() - 1) {
        if let Some(a) = args[i].strip_prefix("--") {
            if a == arg {
                return Some(args[i + 1].clone());
            }
        }
    }
    None
}

#[derive(Deserialize)]
pub struct RustToolchainToml {
    pub toolchain: Option<ToolchainConfig>,
}

#[derive(Deserialize)]
pub struct ToolchainConfig {
    pub channel: Option<String>,
}

pub fn detect_toolchain() -> Result<Option<String>, Box<dyn std::error::Error>> {
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
