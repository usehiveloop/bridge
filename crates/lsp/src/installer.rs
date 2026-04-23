//! LSP Server Installer
//!
//! Handles installation of LSP servers via various package managers.
//! Runs asynchronously in the background when bridge starts with --install-lsp-servers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tracing::{debug, info, warn};

use crate::installer_checksums;

/// Installation method for an LSP server.
///
/// Only package managers that are broadly available on typical Linux/macOS
/// dev boxes are supported here. Servers whose only distribution is via
/// rare toolchains (opam, luarocks, stack, gem, dart pub, coursier, ...)
/// were dropped from `installable_servers()` because attempting to install
/// them reliably fails with `No such file or directory` on boxes that
/// haven't pre-installed the toolchain.
#[derive(Debug, Clone)]
pub enum InstallMethod {
    /// Install via npm: `npm install -g <package>`
    Npm { package: String },
    /// Install via cargo: `cargo install <crate>`
    Cargo { crate_name: String },
    /// Install via go: `go install <path>@latest`
    Go { path: String },
    /// Install via pip: `pip install <package>`
    Pip { package: String },
    /// Custom install command (usually a curl/wget + unpack script).
    Custom { command: String, args: Vec<String> },
    /// Download a pinned URL, verify its SHA256 against
    /// [`installer_checksums::CHECKSUMS`], then hand off to a shell command
    /// that extracts/installs the downloaded file.
    ///
    /// The downloaded path is exposed to the command through the
    /// `BRIDGE_LSP_DL` environment variable (not as an argv slot) so the
    /// shell snippet cannot be tricked into injecting it as a flag.
    DownloadAndRun {
        url: String,
        /// Shell snippet to run with `bash -c`. Receives the downloaded
        /// file path as `$BRIDGE_LSP_DL`.
        shell: String,
    },
}

/// Information about an installable LSP server
#[derive(Debug, Clone)]
pub struct InstallableServer {
    /// Server ID (e.g., "typescript", "rust")
    pub id: String,
    /// Installation method
    pub method: InstallMethod,
    /// Binary name(s) to check if already installed
    pub binaries: Vec<String>,
    /// Description of the server
    pub description: String,
}

/// Returns all installable LSP servers
pub fn installable_servers() -> Vec<InstallableServer> {
    vec![
        // JavaScript/TypeScript
        InstallableServer {
            id: "typescript".to_string(),
            method: InstallMethod::Npm {
                package: "typescript-language-server".to_string(),
            },
            binaries: vec!["typescript-language-server".to_string()],
            description: "TypeScript/JavaScript language server".to_string(),
        },
        InstallableServer {
            id: "eslint".to_string(),
            method: InstallMethod::Npm {
                package: "eslint".to_string(),
            },
            binaries: vec!["eslint".to_string()],
            description: "ESLint LSP server".to_string(),
        },
        InstallableServer {
            id: "biome".to_string(),
            method: InstallMethod::Npm {
                package: "@biomejs/biome".to_string(),
            },
            binaries: vec!["biome".to_string()],
            description: "Biome LSP server for JS/TS/JSON/CSS".to_string(),
        },
        // Deno — pinned to a specific release tarball so the bytes we install
        // are hash-verifiable.
        InstallableServer {
            id: "deno".to_string(),
            method: InstallMethod::DownloadAndRun {
                url: "https://github.com/denoland/deno/releases/download/v2.0.6/deno-x86_64-unknown-linux-gnu.zip".to_string(),
                shell: "set -eu; mkdir -p \"$HOME/.local/bin\"; \
                        unzip -q -o \"$BRIDGE_LSP_DL\" -d \"$HOME/.local/bin\"; \
                        chmod +x \"$HOME/.local/bin/deno\"".to_string(),
            },
            binaries: vec!["deno".to_string()],
            description: "Deno language server (built into the Deno CLI)".to_string(),
        },
        // Web frameworks
        InstallableServer {
            id: "vue".to_string(),
            method: InstallMethod::Npm {
                package: "@vue/language-server".to_string(),
            },
            binaries: vec!["vue-language-server".to_string()],
            description: "Vue language server".to_string(),
        },
        InstallableServer {
            id: "svelte".to_string(),
            method: InstallMethod::Npm {
                package: "svelte-language-server".to_string(),
            },
            binaries: vec!["svelteserver".to_string()],
            description: "Svelte language server".to_string(),
        },
        InstallableServer {
            id: "astro".to_string(),
            method: InstallMethod::Npm {
                package: "@astrojs/language-server".to_string(),
            },
            binaries: vec!["astro-ls".to_string()],
            description: "Astro language server".to_string(),
        },
        // Rust — pinned rust-analyzer 2025-10-27 prebuilt binary. `cargo install`
        // builds from source and takes ~10 minutes; the release binary is a
        // few-megabyte download.
        InstallableServer {
            id: "rust".to_string(),
            method: InstallMethod::DownloadAndRun {
                url: "https://github.com/rust-lang/rust-analyzer/releases/download/2025-10-27/rust-analyzer-x86_64-unknown-linux-gnu.gz".to_string(),
                shell: "set -eu; mkdir -p ~/.local/bin; \
                        gunzip -c \"$BRIDGE_LSP_DL\" > ~/.local/bin/rust-analyzer; \
                        chmod +x ~/.local/bin/rust-analyzer".to_string(),
            },
            binaries: vec!["rust-analyzer".to_string()],
            description: "Rust analyzer".to_string(),
        },
        // Go
        InstallableServer {
            id: "go".to_string(),
            method: InstallMethod::Go {
                path: "golang.org/x/tools/gopls@latest".to_string(),
            },
            binaries: vec!["gopls".to_string()],
            description: "Go language server".to_string(),
        },
        // Python
        InstallableServer {
            id: "python".to_string(),
            method: InstallMethod::Npm {
                package: "pyright".to_string(),
            },
            binaries: vec!["pyright-langserver".to_string()],
            description: "Pyright language server".to_string(),
        },
        // PHP
        InstallableServer {
            id: "php".to_string(),
            method: InstallMethod::Npm {
                package: "intelephense".to_string(),
            },
            binaries: vec!["intelephense".to_string()],
            description: "PHP language server".to_string(),
        },
        // Bash
        InstallableServer {
            id: "bash".to_string(),
            method: InstallMethod::Npm {
                package: "bash-language-server".to_string(),
            },
            binaries: vec!["bash-language-server".to_string()],
            description: "Bash language server".to_string(),
        },
        // Java/Kotlin — Eclipse JDT pinned to a specific dated snapshot.
        InstallableServer {
            id: "jdtls".to_string(),
            method: InstallMethod::DownloadAndRun {
                url: "https://download.eclipse.org/jdtls/snapshots/jdt-language-server-1.45.0-202511062216.tar.gz".to_string(),
                shell: "set -eu; mkdir -p ~/.local/share/jdtls ~/.local/bin; \
                        tar -xzf \"$BRIDGE_LSP_DL\" -C ~/.local/share/jdtls; \
                        ln -sf ~/.local/share/jdtls/bin/jdtls ~/.local/bin/jdtls".to_string(),
            },
            binaries: vec!["jdtls".to_string()],
            description: "Eclipse JDT Language Server".to_string(),
        },
        // C/C++ — apt install clangd. Requires the sandbox to run as root (or
        // passwordless sudo); the Dev-Box image ships with this.
        InstallableServer {
            id: "clangd".to_string(),
            method: InstallMethod::Custom {
                command: "bash".to_string(),
                args: vec![
                    "-c".to_string(),
                    "set -eu; export DEBIAN_FRONTEND=noninteractive; \
                     if command -v sudo >/dev/null 2>&1 && [ \"$(id -u)\" != 0 ]; then \
                         sudo apt-get update -qq && sudo apt-get install -y --no-install-recommends clangd; \
                     else \
                         apt-get update -qq && apt-get install -y --no-install-recommends clangd; \
                     fi".to_string(),
                ],
            },
            binaries: vec!["clangd".to_string()],
            description: "Clangd C/C++ language server".to_string(),
        },
        // Zig — pinned zls 0.14.0 Linux x86_64 tarball.
        InstallableServer {
            id: "zig".to_string(),
            method: InstallMethod::DownloadAndRun {
                url: "https://github.com/zigtools/zls/releases/download/0.14.0/zls-linux-x86_64.tar.gz".to_string(),
                shell: "set -eu; mkdir -p ~/.local/share/zls ~/.local/bin; \
                        tar -xzf \"$BRIDGE_LSP_DL\" -C ~/.local/share/zls; \
                        ln -sf ~/.local/share/zls/zls ~/.local/bin/zls".to_string(),
            },
            binaries: vec!["zls".to_string()],
            description: "Zig language server".to_string(),
        },
        // Terraform — pinned 0.39.0 to avoid the floating GitHub API lookup.
        InstallableServer {
            id: "terraform".to_string(),
            method: InstallMethod::DownloadAndRun {
                url: "https://releases.hashicorp.com/terraform-ls/0.39.0/terraform-ls_0.39.0_linux_amd64.zip".to_string(),
                shell: "set -eu; mkdir -p ~/.local/bin; \
                        unzip -q -o \"$BRIDGE_LSP_DL\" -d ~/.local/bin/; \
                        chmod +x ~/.local/bin/terraform-ls".to_string(),
            },
            binaries: vec!["terraform-ls".to_string()],
            description: "Terraform language server".to_string(),
        },
        // Dockerfile
        InstallableServer {
            id: "dockerfile".to_string(),
            method: InstallMethod::Npm {
                package: "dockerfile-language-server-nodejs".to_string(),
            },
            binaries: vec!["dockerfile-language-server-nodejs".to_string()],
            description: "Dockerfile language server".to_string(),
        },
        // YAML
        InstallableServer {
            id: "yaml-ls".to_string(),
            method: InstallMethod::Npm {
                package: "yaml-language-server".to_string(),
            },
            binaries: vec!["yaml-language-server".to_string()],
            description: "YAML language server".to_string(),
        },
        // Prisma
        InstallableServer {
            id: "prisma".to_string(),
            method: InstallMethod::Npm {
                package: "@prisma/language-server".to_string(),
            },
            binaries: vec!["prisma-language-server".to_string()],
            description: "Prisma language server".to_string(),
        },
        // Elm
        InstallableServer {
            id: "elm".to_string(),
            method: InstallMethod::Npm {
                package: "@elm-tooling/elm-language-server".to_string(),
            },
            binaries: vec!["elm-language-server".to_string()],
            description: "Elm language server".to_string(),
        },
        // Elixir — pinned to 0.23.1.
        InstallableServer {
            id: "elixir-ls".to_string(),
            method: InstallMethod::DownloadAndRun {
                url: "https://github.com/elixir-lsp/elixir-ls/releases/download/v0.23.1/elixir-ls-v0.23.1.zip".to_string(),
                shell: "set -eu; mkdir -p ~/.local/share/elixir-ls ~/.local/bin; \
                        unzip -q -o \"$BRIDGE_LSP_DL\" -d ~/.local/share/elixir-ls; \
                        chmod +x ~/.local/share/elixir-ls/language_server.sh; \
                        ln -sf ~/.local/share/elixir-ls/language_server.sh ~/.local/bin/language_server.sh".to_string(),
            },
            binaries: vec!["language_server.sh".to_string()],
            description: "Elixir language server".to_string(),
        },
        // Clojure — pinned to 2025.10.24 native build.
        InstallableServer {
            id: "clojure-lsp".to_string(),
            method: InstallMethod::DownloadAndRun {
                url: "https://github.com/clojure-lsp/clojure-lsp/releases/download/2025.10.24-15.44.27/clojure-lsp-native-linux-amd64.zip".to_string(),
                shell: "set -eu; mkdir -p ~/.local/bin; \
                        unzip -q -o \"$BRIDGE_LSP_DL\" -d ~/.local/bin/; \
                        chmod +x ~/.local/bin/clojure-lsp".to_string(),
            },
            binaries: vec!["clojure-lsp".to_string()],
            description: "Clojure language server".to_string(),
        },
        // Typst
        InstallableServer {
            id: "tinymist".to_string(),
            method: InstallMethod::Cargo {
                crate_name: "tinymist".to_string(),
            },
            binaries: vec!["tinymist".to_string()],
            description: "Typst language server".to_string(),
        },
        // Python - Ruff (very fast linter/formatter with LSP)
        InstallableServer {
            id: "ruff".to_string(),
            method: InstallMethod::Pip {
                package: "ruff-lsp".to_string(),
            },
            binaries: vec!["ruff-lsp".to_string()],
            description: "Ruff Python LSP (fast linter/formatter)".to_string(),
        },
        // Python - python-lsp-server (alternative to pyright)
        InstallableServer {
            id: "pylsp".to_string(),
            method: InstallMethod::Pip {
                package: "python-lsp-server".to_string(),
            },
            binaries: vec!["pylsp".to_string()],
            description: "Python LSP Server (alternative to pyright)".to_string(),
        },
        // Tailwind CSS
        InstallableServer {
            id: "tailwindcss".to_string(),
            method: InstallMethod::Npm {
                package: "@tailwindcss/language-server".to_string(),
            },
            binaries: vec!["tailwindcss-language-server".to_string()],
            description: "Tailwind CSS language server".to_string(),
        },
        // GraphQL
        InstallableServer {
            id: "graphql".to_string(),
            method: InstallMethod::Npm {
                package: "graphql-language-service-cli".to_string(),
            },
            binaries: vec!["graphql-lsp".to_string()],
            description: "GraphQL language server".to_string(),
        },
        // CMake
        InstallableServer {
            id: "cmake".to_string(),
            method: InstallMethod::Pip {
                package: "cmake-language-server".to_string(),
            },
            binaries: vec!["cmake-language-server".to_string()],
            description: "CMake language server".to_string(),
        },
        // Ansible
        InstallableServer {
            id: "ansible".to_string(),
            method: InstallMethod::Pip {
                package: "ansible-language-server".to_string(),
            },
            binaries: vec!["ansible-language-server".to_string()],
            description: "Ansible language server".to_string(),
        },
        // VimScript
        InstallableServer {
            id: "vimls".to_string(),
            method: InstallMethod::Npm {
                package: "vim-language-server".to_string(),
            },
            binaries: vec!["vim-language-server".to_string()],
            description: "VimScript language server".to_string(),
        },
    ]
}

/// LSP Installer handles installation of language servers
pub struct LspInstaller {
    servers: HashMap<String, InstallableServer>,
}

impl LspInstaller {
    /// Create a new installer with all available servers
    pub fn new() -> Self {
        let servers: HashMap<String, InstallableServer> = installable_servers()
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        Self { servers }
    }

    /// Get list of all installable server IDs
    pub fn available_servers(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    /// Check if a binary exists in PATH
    async fn binary_exists(&self, binary: &str) -> bool {
        match tokio::process::Command::new("which")
            .arg(binary)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
            Ok(status) => status.success(),
            Err(_) => false,
        }
    }

    /// Install a single server by ID
    async fn install_server(&self, server_id: &str) -> Result<(), String> {
        let server = self
            .servers
            .get(server_id)
            .ok_or_else(|| format!("Unknown LSP server: {}", server_id))?;

        // Check if already installed
        for binary in &server.binaries {
            if self.binary_exists(binary).await {
                info!(server = %server_id, binary = %binary, "already installed, skipping");
                return Ok(());
            }
        }

        info!(server = %server_id, method = ?server.method, "installing LSP server");

        let result = match &server.method {
            InstallMethod::Npm { package } => self.install_npm(package).await,
            InstallMethod::Cargo { crate_name } => self.install_cargo(crate_name).await,
            InstallMethod::Go { path } => self.install_go(path).await,
            InstallMethod::Pip { package } => self.install_pip(package).await,
            InstallMethod::Custom { command, args } => self.install_custom(command, args).await,
            InstallMethod::DownloadAndRun { url, shell } => {
                self.install_download_and_run(url, shell).await
            }
        };

        match result {
            Ok(_) => {
                info!(server = %server_id, "installation complete");
                Ok(())
            }
            Err(e) => {
                // Downgraded from error! → warn!: a single missing toolchain
                // (opam, dotnet, gem, ...) should not make `bridge install-lsp
                // all` look catastrophic. The CLI surfaces a summary at the
                // end and always exits 0; the operator can install the
                // underlying toolchain and re-run for the specific id.
                warn!(server = %server_id, error = %e, "installation failed");
                Err(e)
            }
        }
    }

    /// Install servers by IDs (or "all" for all servers)
    pub async fn install(&self, server_ids: &[String]) -> HashMap<String, Result<(), String>> {
        let ids_to_install: Vec<String> = if server_ids.contains(&"all".to_string()) {
            self.available_servers()
        } else {
            server_ids.to_vec()
        };

        let mut results = HashMap::new();

        for id in ids_to_install {
            let result = self.install_server(&id).await;
            results.insert(id, result);
        }

        results
    }

    /// Run an installer command, capturing stderr and surfacing it on failure.
    async fn run_install_cmd(
        &self,
        program: &str,
        args: &[&str],
        label: &str,
    ) -> Result<(), String> {
        let output = tokio::process::Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("Failed to run {}: {}", program, e))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr
                .lines()
                .rev()
                .take(10)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            let tail = if tail.is_empty() {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" | ")
            } else {
                tail
            };
            Err(format!(
                "{} install failed for {}: {}",
                program,
                label,
                tail.trim()
            ))
        }
    }

    /// Install npm package globally
    async fn install_npm(&self, package: &str) -> Result<(), String> {
        debug!(package = %package, "running npm install");
        self.run_install_cmd("npm", &["install", "-g", package], package)
            .await
    }

    /// Install cargo crate
    async fn install_cargo(&self, crate_name: &str) -> Result<(), String> {
        debug!(crate_name = %crate_name, "running cargo install");
        self.run_install_cmd("cargo", &["install", crate_name], crate_name)
            .await
    }

    /// Install go package
    async fn install_go(&self, path: &str) -> Result<(), String> {
        debug!(path = %path, "running go install");
        self.run_install_cmd("go", &["install", path], path).await
    }

    /// Install pip package. Uses `python3 -m pip install --user
    /// --break-system-packages` because (a) bare `pip` is missing on modern
    /// systems, (b) PEP 668-marked distros (Homebrew Python, recent Debian)
    /// reject `pip install` without the explicit override.
    async fn install_pip(&self, package: &str) -> Result<(), String> {
        debug!(package = %package, "running python3 -m pip install");
        self.run_install_cmd(
            "python3",
            &[
                "-m",
                "pip",
                "install",
                "--user",
                "--break-system-packages",
                package,
            ],
            package,
        )
        .await
    }

    /// Run custom install command.
    ///
    /// The `command` string itself is validated to reject shell metacharacters —
    /// args are passed to `Command::args()` so they're safe by construction,
    /// but letting an attacker squirrel a `;` or `$(...)` into the *program*
    /// name would allow escaping out of the exec path.
    async fn install_custom(&self, command: &str, args: &[String]) -> Result<(), String> {
        validate_command_name(command)?;
        debug!(command = %command, args = ?args, "running custom install");
        let status = tokio::process::Command::new(command)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .await
            .map_err(|e| format!("Failed to run {}: {}", command, e))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("custom install command failed: {}", command))
        }
    }

    /// Download a pinned URL to a scratch file, verify its SHA256 against the
    /// pinned checksum registry, then exec the provided shell snippet with
    /// `$BRIDGE_LSP_DL` set to the downloaded path.
    async fn install_download_and_run(&self, url: &str, shell: &str) -> Result<(), String> {
        debug!(url = %url, "downloading pinned LSP binary");
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("reqwest client init: {}", e))?;
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("download failed for {}: {}", url, e))?;
        if !resp.status().is_success() {
            return Err(format!(
                "download failed for {}: HTTP {}",
                url,
                resp.status()
            ));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("read body for {}: {}", url, e))?;

        // Verify checksum. Known hash → must match. "TODO" / unknown URL →
        // logged warning inside `verify`, then Ok.
        installer_checksums::verify(url, &bytes)?;

        // Write to a unique temp path so concurrent installs don't collide.
        let tmp_path: PathBuf = std::env::temp_dir().join(format!(
            "bridge-lsp-dl-{}",
            uuid_like_scratch_name(url)
        ));
        tokio::fs::write(&tmp_path, &bytes)
            .await
            .map_err(|e| format!("write {}: {}", tmp_path.display(), e))?;

        let output = tokio::process::Command::new("bash")
            .args(["-c", shell])
            .env("BRIDGE_LSP_DL", &tmp_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("bash spawn failed: {}", e))?;

        // Best-effort cleanup — leaving the scratch file around on failure is
        // fine, the temp dir gets cleared on reboot.
        let _ = tokio::fs::remove_file(&tmp_path).await;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "install shell failed for {}: {}",
                url,
                stderr.trim()
            ))
        }
    }
}

/// Reject command strings that contain shell metacharacters that could break
/// out of the exec contract. Arg values are safe because `Command::args` does
/// not shell-interpret, but the `command` is passed directly to `execvp`
/// only on the assumption it's a plain program name.
fn validate_command_name(command: &str) -> Result<(), String> {
    const FORBIDDEN: &[&str] = &[";", "|", "&", "`", "$(", "\n", "\r", "\0", "<", ">"];
    for pat in FORBIDDEN {
        if command.contains(pat) {
            return Err(format!(
                "install command contains forbidden metacharacter '{}': {:?}",
                pat, command
            ));
        }
    }
    Ok(())
}

/// Derive a deterministic scratch-file name from a URL without pulling in a
/// uuid dep. Collisions are irrelevant — we just need uniqueness per URL.
fn uuid_like_scratch_name(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    let d = h.finalize();
    let mut s = String::with_capacity(16);
    for b in &d[..8] {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

impl Default for LspInstaller {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_installable_servers_list() {
        let servers = installable_servers();
        assert!(!servers.is_empty(), "should have installable servers");

        // Check that popular servers are included
        let ids: Vec<&str> = servers.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"typescript"), "should include typescript");
        assert!(ids.contains(&"rust"), "should include rust");
        assert!(ids.contains(&"go"), "should include go");
        assert!(ids.contains(&"python"), "should include python");
    }

    #[test]
    fn test_installer_new() {
        let installer = LspInstaller::new();
        let available = installer.available_servers();
        assert!(!available.is_empty(), "should have available servers");
        assert!(available.contains(&"typescript".to_string()));
    }

    #[test]
    fn test_validate_command_name_accepts_plain_binary() {
        assert!(validate_command_name("bash").is_ok());
        assert!(validate_command_name("/usr/bin/env").is_ok());
        assert!(validate_command_name("npm").is_ok());
    }

    #[test]
    fn test_validate_command_name_rejects_metacharacters() {
        for bad in &[
            "bash;whoami",
            "bash|cat",
            "bash&echo",
            "bash`id`",
            "bash$(id)",
            "bash\nwhoami",
            "bash\0",
            "bash<etc",
            "bash>out",
        ] {
            assert!(
                validate_command_name(bad).is_err(),
                "expected rejection for {:?}",
                bad
            );
        }
    }

    #[test]
    fn test_download_methods_have_pinned_checksums_or_todo() {
        // Every DownloadAndRun URL must resolve in the checksum registry,
        // even if the entry is still TODO. Prevents silent downgrade to the
        // "no pinned checksum" warn path.
        for server in installable_servers() {
            if let InstallMethod::DownloadAndRun { url, .. } = &server.method {
                assert!(
                    crate::installer_checksums::lookup(url).is_some(),
                    "URL {} is pinned in installer but missing from CHECKSUMS",
                    url
                );
            }
        }
    }

    #[test]
    fn test_server_methods() {
        let servers = installable_servers();

        // Check various install methods are represented
        let has_npm = servers
            .iter()
            .any(|s| matches!(s.method, InstallMethod::Npm { .. }));
        let has_cargo = servers
            .iter()
            .any(|s| matches!(s.method, InstallMethod::Cargo { .. }));
        let has_go = servers
            .iter()
            .any(|s| matches!(s.method, InstallMethod::Go { .. }));

        assert!(has_npm, "should have npm-based servers");
        assert!(has_cargo, "should have cargo-based servers");
        assert!(has_go, "should have go-based servers");
    }
}
