# cargo-offload

A high-performance CLI tool for offloading Rust compilation to remote servers. Speed up your build times by leveraging powerful remote machines while keeping your development environment lightweight.

## 🚀 Features

- **Remote Compilation**: Build your Rust projects on powerful remote servers
- **Seamless Integration**: Works as a drop-in replacement for common `cargo` commands
- **Intelligent Syncing**: Efficiently syncs only necessary source files using `rsync`
- **Toolchain Management**: Automatically detects and sets up the correct Rust toolchain on remote servers
- **Multi-target Support**: Build for different target architectures
- **Workspace Support**: Full support for Rust workspaces and multi-binary projects
- **Parallel Binary Transfer**: Efficiently copies multiple binaries in parallel
- **Clean Integration**: Mimics standard `cargo` command behavior

## 📋 Prerequisites

- **Local Machine**: Rust toolchain, `rsync`, and `ssh` client
- **Remote Server**: Rust toolchain, `ssh` server, and network accessibility
- **SSH Access**: Passwordless SSH access to the remote server (using SSH keys)

## 🔧 Installation

### From Source

```bash
git clone https://github.com/your-username/offload.git
cd offload
cargo install --path .
```

### Using Cargo

```bash
cargo install --git https://github.com/patrickjeremic/offload
```

## ⚙️ Setup

### 1. SSH Key Authentication

Ensure you have passwordless SSH access to your remote server:

```bash
# Generate SSH key if you don't have one
ssh-keygen -t ed25519 -C "your_email@example.com"

# Copy your public key to the remote server
ssh-copy-id user@remote-server.com
```

### 2. Remote Server Setup

Install Rust on your remote server:

```bash
# On the remote server
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 3. Environment Configuration

Set your default remote host (optional):

```bash
export CARGO_OFFLOAD_HOST=user@remote-server.com:22
```

Or create a shell alias for convenience:

```bash
alias co="offload --host user@remote-server.com"
```

## 🎯 Usage

### Basic Commands

#### Build
Compile your project on the remote server and copy binaries back:

```bash
offload build
offload build --release
offload build --bin my-binary
```

#### Run
Build on remote server and run the binary locally:

```bash
offload run
offload run --bin my-binary
offload run --release -- --config app.toml
```

#### Test
Run tests on the remote server:

```bash
offload test
offload test --lib
offload test integration_tests
```

#### Clippy
Run Clippy linting on the remote server:

```bash
offload clippy
offload clippy -- -D warnings
```

#### Clean
Clean both remote and local build artifacts:

```bash
offload clean
```

#### Toolchain
Manage Rust toolchains on the remote server:

```bash
offload toolchain install stable
offload toolchain list
```

### Global Options

All commands support these global options:

- `--host, -h <HOST>`: SSH host (user@hostname or hostname)
- `--port, -p <PORT>`: SSH port (default: 22)
- `--target <TARGET>`: Target triple (default: x86_64-unknown-linux-gnu)

### Examples

```bash
# Build with specific host and port
offload --host user@build-server.com --port 2222 build --release

# Cross-compile for a different target
offload --target aarch64-unknown-linux-gnu build

# Run with arguments separated by --
offload run --bin server -- --port 8080 --config production.toml

# Test with specific host from environment
CARGO_OFFLOAD_HOST=developer@ci-server.com offload test
```

### Toolchain Detection

`offload` automatically detects your project's Rust toolchain from:

1. `rust-toolchain.toml` file
2. `rust-toolchain` file
3. Falls back to the remote server's default toolchain

Example `rust-toolchain.toml`:
```toml
[toolchain]
channel = "1.70.0"
```

### Using Toolchain Override

You can specify a toolchain using the `+toolchain` syntax:

```bash
offload +nightly build
offload +1.70.0 test
```

## 🏗️ How It Works

1. **Source Sync**: Uses `rsync` to efficiently sync your source code to `/tmp/offload/[project-name]` on the remote server
2. **Toolchain Setup**: Installs and configures the required Rust toolchain on the remote server
3. **Remote Build**: Executes the cargo command on the remote server with proper target configuration
4. **Binary Transfer**: Copies compiled binaries back to `target/offload/[target]/[profile]/` in your local project
5. **Local Execution**: For `run` commands, executes the binary locally with provided arguments

## 📁 Directory Structure

Local directories created by `offload`:

```
your-project/
├── target/
│   └── offload/
│       └── x86_64-unknown-linux-gnu/
│           ├── debug/
│           │   └── your-binary
│           └── release/
│               └── your-binary
└── ...
```

Remote directory structure:

```
/tmp/offload/
└── your-project/
    ├── src/
    ├── Cargo.toml
    ├── target/
    └── ...
```

## 🔍 Environment Variables

- `CARGO_OFFLOAD_HOST`: Default SSH host (can include port like `user@host:port`)
- `RUST_LOG`: Control logging verbosity (e.g., `RUST_LOG=debug`)

## ⚡ Performance Tips

1. **Use SSH Connection Multiplexing**: Add to your `~/.ssh/config`:
   ```
   Host your-build-server
       ControlMaster auto
       ControlPath ~/.ssh/master-%r@%h:%p
       ControlPersist 10m
   ```

2. **Optimize rsync**: The tool automatically excludes common directories like `target/`, `.git/`, but you can further optimize by maintaining a clean source directory.

3. **Remote Server Specs**: Use servers with:
   - Fast CPUs (high core count for parallel compilation)
   - Sufficient RAM (4GB+ for large projects)
   - Fast storage (SSD preferred)

## 🐛 Troubleshooting

### Common Issues

**SSH Connection Failed**
```bash
# Test SSH connection manually
ssh user@remote-server.com

# Check SSH key authentication
ssh -v user@remote-server.com
```

**Rsync Permission Errors**
```bash
# Ensure write permissions to /tmp on remote server
ssh user@remote-server.com "ls -la /tmp"
```

**Missing Toolchain**
```bash
# Install toolchain manually on remote server
offload toolchain install stable
```

**Binary Not Found**
- Ensure your project compiles successfully locally first
- Check that the binary target exists in your `Cargo.toml`

### Debug Mode

Enable debug logging for troubleshooting:

```bash
RUST_LOG=debug offload build
```

## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request. For major changes, please open an issue first to discuss what you would like to change.

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- Inspired by the need for faster Rust compilation in resource-constrained environments
- Built with [clap](https://crates.io/crates/clap) for CLI parsing
- Uses standard tools like `rsync` and `ssh` for reliable file transfer and remote execution

---

**Happy remote building! 🦀🚀**
