# cargo-offload

A high-performance CLI tool for offloading Rust compilation to remote servers. Speed up your build times by leveraging powerful remote machines while keeping your development environment lightweight.

## 🚀 Features

- **Remote Compilation**: Build your Rust projects on powerful remote servers
- **Flexible Execution**: Run binaries locally or remotely with port forwarding support
- **Seamless Integration**: Works as a drop-in replacement for common `cargo` commands
- **Intelligent Syncing**: Efficiently syncs only necessary source files using `rsync`
- **Toolchain Management**: Automatically detects and sets up the correct Rust toolchain on remote servers
- **Multi-target Support**: Build for different target architectures
- **Workspace Support**: Full support for Rust workspaces and multi-binary projects
- **Parallel Binary Transfer**: Efficiently copies multiple binaries in parallel
- **SSH Port Forwarding**: Forward ports from remote to local for network services
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

#### Run (Local Execution)
Build on remote server and run the binary locally:

```bash
offload run
offload run --bin my-binary
offload run --release -- --config app.toml
```

#### Run Local (Explicit)
Same as `run` - build remotely and execute locally:

```bash
offload run-local
offload run-local --bin my-binary -- --verbose
```

#### Run Remote
Execute `cargo run` directly on the remote server with optional port forwarding:

```bash
# Basic remote execution
offload run-remote

# With port forwarding (same port on both sides)
offload --forward 8080 run-remote -- --bin web-server

# With different local and remote ports
offload --forward 3000:8080 run-remote -- --release

# Multiple port forwards
offload --forward 8080:8080 --forward 5432:5432 run-remote -- --config prod.yaml

# Short flag syntax
offload -L 8080:3000 run-remote -- --bin api-server
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
- `--env, -e <ENV>`: Environment variables to pass to remote cargo commands (can be specified multiple times)
- `--copy-all-artifacts`: Copy all artifacts from target directory (including deps, build, etc.)
- `--forward, -L <PORT_SPEC>`: Forward ports from remote to local (format: `local_port:remote_port` or just `port`)

### Port Forwarding

The `--forward` (or `-L`) flag enables SSH port forwarding from the remote server to your local machine. This is particularly useful when running web servers, databases, or other network services remotely.

#### Port Forwarding Formats

```bash
# Same port on both local and remote (8080 -> 8080)
offload --forward 8080 run-remote

# Different ports (local 3000 -> remote 8080)
offload --forward 3000:8080 run-remote

# Multiple port forwards
offload --forward 8080:8080 --forward 5432:5432 --forward 6379:6379 run-remote
```

#### Use Cases

- **Web Development**: Forward HTTP/HTTPS ports for web applications
- **Database Development**: Forward database ports (PostgreSQL, MySQL, Redis, etc.)
- **API Development**: Forward API server ports for local testing
- **Microservices**: Forward multiple service ports simultaneously

#### Example: Web Server Development

```bash
# Run a web server on remote port 8080, accessible locally on port 3000
offload --forward 3000:8080 run-remote -- --bin web-server --port 8080

# Now you can access the remote server at http://localhost:3000
```

### Examples

```bash
# Build with specific host and port
offload --host user@build-server.com --port 2222 build --release

# Cross-compile for a different target
offload --target aarch64-unknown-linux-gnu build

# Run with arguments separated by --
offload run --bin server -- --port 8080 --config production.toml

# Run remotely with port forwarding for a web API
offload --forward 8080:8080 run-remote -- --bin api-server --host 0.0.0.0

# Test with specific host from environment
CARGO_OFFLOAD_HOST=developer@ci-server.com offload test

# Remote development with database and web server
offload --forward 3000:8080 --forward 5432:5432 run-remote -- --bin full-stack-app
```

### Command Comparison

| Command | Build Location | Execution Location | Binary Transfer | Port Forwarding |
|---------|----------------|-------------------|-----------------|-----------------|
| `build` | Remote | N/A | Yes | No |
| `run` | Remote | Local | Yes | No |
| `run-local` | Remote | Local | Yes | No |
| `run-remote` | Remote | Remote | No | Yes (optional) |

### Artifact Copying

By default, `offload` copies only the necessary artifacts from the remote target directory, excluding large build directories:

- Copies the entire `target/{target}/{profile}/` directory structure
- Excludes `build/`, `deps/`, and `incremental/` subdirectories to minimize transfer size
- Makes binaries and examples executable automatically

To copy all artifacts including dependencies and build files:

```bash
offload --copy-all-artifacts build
```

This is useful when:
- You need the complete build artifacts for debugging
- You're working with custom build scripts that generate files in these directories
- You want to preserve incremental compilation data

### Environment Variables for Remote Builds

You can pass environment variables to the remote cargo command using the `-e` or `--env` flag:

```bash
# Specify C compiler for the build
offload -e CC=gcc-13 -e CXX=g++-13 build

# Set Rust-specific environment variables
offload -e RUST_BACKTRACE=1 -e RUST_LOG=debug test

# Configure compiler flags 
offload -e RUSTFLAGS="-C target-cpu=native" build --release
offload -e CXXFLAGS="-include cstdint" build

# Combine with other options
offload --host build-server.com -e RUSTFLAGS="-D warnings" clippy

# Use with run command
offload -e CARGO_TERM_COLOR=always run -- --verbose

# Multiple environment variables with complex values
offload -e CC=gcc-13 -e CFLAGS="-O3 -march=native" -e RUSTFLAGS="-C target-feature=+avx2" build

# Environment variables with remote execution and port forwarding
offload -e RUST_LOG=debug --forward 8080 run-remote -- --bin web-server
```

These environment variables are only applied to the cargo command on the remote machine and don't affect your local environment. Values containing spaces, quotes, or special characters are properly escaped to ensure they work correctly on the remote system.

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
offload +nightly run-remote -- --bin experimental-feature
```

## 🏗️ How It Works

### Local Execution (`run`, `run-local`)
1. **Source Sync**: Uses `rsync` to efficiently sync your source code to `/tmp/offload/[project-name]` on the remote server
2. **Toolchain Setup**: Installs and configures the required Rust toolchain on the remote server
3. **Remote Build**: Executes the cargo command on the remote server with proper target configuration
4. **Binary Transfer**: Copies compiled binaries back to `target/offload/[target]/[profile]/` in your local project
5. **Local Execution**: Executes the binary locally with provided arguments

### Remote Execution (`run-remote`)
1. **Source Sync**: Uses `rsync` to efficiently sync your source code to the remote server
2. **Toolchain Setup**: Installs and configures the required Rust toolchain on the remote server
3. **Remote Execution**: Executes `cargo run` directly on the remote server with SSH port forwarding
4. **Port Forwarding**: Maintains SSH tunnel for specified ports throughout execution
5. **Interactive Support**: Provides full terminal interaction with the remote process

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

4. **Choose Execution Mode Wisely**:
   - Use `run-local` for CPU-intensive applications that don't need network services
   - Use `run-remote` with port forwarding for web applications, APIs, and network services
   - Use `run-remote` without port forwarding for CLI tools and batch processing

## 🐛 Troubleshooting

### Common Issues

**SSH Connection Failed**
```bash
# Test SSH connection manually
ssh user@remote-server.com

# Check SSH key authentication
ssh -v user@remote-server.com
```

**Port Forwarding Issues**
```bash
# Check if port is already in use locally
netstat -an | grep LISTEN | grep :8080

# Test port forwarding manually
ssh -L 8080:localhost:8080 user@remote-server.com

# Use different local port if there's a conflict
offload --forward 3000:8080 run-remote
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
RUST_LOG=debug offload --forward 8080 run-remote
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
