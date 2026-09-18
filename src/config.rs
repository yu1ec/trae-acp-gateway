//! Gateway configuration (CLI args only, per requirements).

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "trae_acp_gateway", about = "OpenAI-compatible gateway over an ACP agent (trae CLI)")]
pub struct Config {
    /// TCP port to listen on (localhost only).
    #[arg(long, default_value_t = 8080)]
    pub port: u16,

    /// Working directory handed to the agent (sessions and tools operate here).
    #[arg(long, default_value = ".")]
    pub workdir: String,

    /// Agent executable to spawn.
    #[arg(long, default_value = "traecli")]
    pub trae_cmd: String,

    /// Arguments passed to the agent executable.
    #[arg(long, value_delimiter = ',', default_value = "acp,serve")]
    pub trae_args: Vec<String>,

    /// Keep the agent inside its sandbox: deny permission requests that
    /// escalate beyond it instead of approving them. Disable with `--sandbox=false`.
    #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
    pub sandbox: bool,

    /// Log every raw JSON-RPC line exchanged with the agent.
    #[arg(long)]
    pub debug: bool,
}
