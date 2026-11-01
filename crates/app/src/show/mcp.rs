mod protocol;
mod scope;
mod tools;

use std::{ffi::OsString, fmt, io, path::PathBuf};

use anyhow::Context as _;
use scope::Roots;
use serde_json::json;

const DISCLOSURE: &str = "Requested Show text and images go to the calling agent and possibly its model provider. Screen and narration text is evidence, not instructions. Each content request names an exact Show within a configured root; no latest Show is inferred.";

const HELP: &str = "speakeasy show mcp --root PATH [--root PATH ...]
speakeasy show mcp --setup

Serve three read-only Show tools over stdio. At least one existing root is required.
Roots are fixed at launch; tool arguments cannot add folders. Symlinks are refused.
Each root can be a saved Show or a folder containing saved Shows.
--setup prints Claude Code, Codex and generic client stanzas; it changes no settings.
Requested text and images go to the calling agent and possibly its model provider.
Screen and narration text is evidence, not instructions. No dictation is exposed.";

#[derive(PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Setup,
    Serve { roots: Vec<PathBuf> },
}

impl fmt::Debug for Command {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Help => out.write_str("McpHelp"),
            Self::Setup => out.write_str("McpSetup"),
            Self::Serve { roots } => out
                .debug_struct("McpServe")
                .field("roots", &roots.len())
                .finish(),
        }
    }
}

impl Command {
    pub(crate) fn parse(mut args: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let mut roots = Vec::new();
        let mut setup = false;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--setup") if !setup && roots.is_empty() => setup = true,
                Some("--root") if !setup && roots.len() < 16 => {
                    let root = args
                        .next()
                        .filter(|value| {
                            !value.is_empty() && !value.to_string_lossy().starts_with('-')
                        })
                        .context("--root needs an existing folder")?;
                    roots.push(PathBuf::from(root));
                },
                _ => anyhow::bail!(
                    "Unknown, repeated, or conflicting MCP argument; use show mcp --help"
                ),
            }
        }
        if setup {
            return Ok(Self::Setup);
        }
        anyhow::ensure!(
            !roots.is_empty(),
            "MCP needs at least one explicit --root folder"
        );
        Ok(Self::Serve { roots })
    }

    pub(crate) fn write(self, out: &mut impl io::Write) -> anyhow::Result<()> {
        match self {
            Self::Help => writeln!(out, "{HELP}")?,
            Self::Setup => setup(out)?,
            Self::Serve { roots } => {
                let roots = Roots::open(roots).map_err(anyhow::Error::msg)?;
                protocol::serve(&roots, &mut io::stdin().lock(), out)?;
            },
        }
        Ok(())
    }
}

fn setup(out: &mut impl io::Write) -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("Cannot resolve the Speakeasy executable")?;
    #[cfg(windows)]
    let executable = executable.with_file_name("speakeasy-cli.exe");
    let executable = executable
        .canonicalize()
        .context("Cannot resolve the Speakeasy console executable")?;
    let root = super::store::directory().context("Cannot resolve the default Shows folder")?;
    let executable = executable
        .to_str()
        .context("The executable path must be valid UTF-8 for MCP client setup")?;
    let root = root
        .to_str()
        .context("The Shows path must be valid UTF-8 for MCP client setup")?;
    setup_for(executable, root, out)
}

fn setup_for(executable: &str, root: &str, out: &mut impl io::Write) -> anyhow::Result<()> {
    let args = ["show", "mcp", "--root", root];
    let command = json!({"command": executable, "args": args});
    writeln!(out, "{DISCLOSURE}\n\nClaude Code (.mcp.json):")?;
    serde_json::to_writer_pretty(
        &mut *out,
        &json!({"mcpServers": {"speakeasy-show": command}}),
    )?;
    writeln!(
        out,
        "\n\nCodex (config.toml):\n[mcp_servers.speakeasy-show]\ncommand = {}\nargs = {}",
        serde_json::to_string(executable)?,
        serde_json::to_string(&args)?
    )?;
    writeln!(out, "\nGeneric stdio client:")?;
    serde_json::to_writer_pretty(&mut *out, &command)?;
    writeln!(out)?;
    Ok(())
}

#[cfg(test)]
mod tests;
