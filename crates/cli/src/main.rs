// Rust guideline compliant 2026-02-21
//! Argus CLI — HTTP client for the supervisor REST API.

mod client;
mod commands;
mod helpers;
mod output;
mod types;

use anyhow::Result;
use clap::Parser;

use client::Client;
use commands::{Command, RulesCommand};

/// Supervisor API address used when ARGUS_URL is unset.
const DEFAULT_URL: &str = "http://127.0.0.1:9090";

#[derive(Parser)]
#[command(name = "argus", about = "Argus filesystem versioning CLI")]
struct Cli {
    /// Supervisor API base URL.
    #[arg(long, default_value = DEFAULT_URL, global = true, env = "ARGUS_URL")]
    url: String,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let c = Client::new(cli.url);

    match cli.command {
        Command::Status => output::print_status(&c.status().await?),
        Command::Pause => output::print_pause(&c.pause().await?),
        Command::Resume => output::print_resume(&c.resume().await?),
        Command::Log { since, path, pid, event_type, limit, json } => {
            let params = helpers::build_event_params(since.as_deref(), path.as_deref(), pid, event_type.as_deref(), limit)?;
            let body = c.events(&params).await?;
            if json { print!("{body}"); } else { output::print_events_human(&body); }
        }
        Command::History { path } => {
            output::print_file_history(&c.file_history(&path).await?);
        }
        Command::Stdio { pid, stream, follow } => {
            if follow {
                let s = stream.as_deref().unwrap_or("stdout");
                let mut resp = c.stdio_follow(pid, s).await?;
                while let Some(chunk) = resp.chunk().await? {
                    let text = String::from_utf8_lossy(&chunk);
                    print!("{text}");
                }
            } else {
                output::print_stdio(&c.stdio(pid, stream.as_deref()).await?);
            }
        }
        Command::Pipeline { shell_pid } => {
            output::print_pipeline(&c.pipeline(shell_pid).await?);
        }
        Command::ProcessTree { root, stdio, depth } => {
            let tree = c.process_tree(root, stdio, depth).await?;
            output::print_process_tree(&tree, 0);
        }
        Command::Cat { hash } => print!("{}", c.cat(&hash).await?),
        Command::Diff { before, after, from, to } => {
            if let (Some(from), Some(to)) = (from, to) {
                output::print_tree_diff(&c.tree_diff(from, to).await?);
            } else if let (Some(before), Some(after)) = (before, after) {
                print!("{}", c.diff(&before, &after).await?);
            } else {
                anyhow::bail!("usage: argus diff <before> <after> OR argus diff --from <seq> --to <seq>");
            }
        }
        Command::Snapshot { seq, path } => {
            output::print_tree(&c.tree(seq, path.as_deref()).await?);
        }
        Command::Restore { timestamp, seq, target, in_place, force, path } => {
            let req = helpers::build_restore_request(timestamp, seq, target, in_place, force, path)?;
            output::print_restore(&c.restore(&req).await?);
        }
        Command::Undo { last, last_by_pid } => {
            let req = types::UndoRequest { last, last_by_pid };
            output::print_restore(&c.restore_undo(&req).await?);
        }
        Command::Connections { pid, active } => {
            output::print_connections(&c.connections(pid, active).await?);
        }
        Command::StorageStatus => {
            output::print_storage_status(&c.storage_status().await?);
        }
        Command::Rules { action } => match action {
            None => output::print_rules(&c.rules().await?),
            Some(RulesCommand::Set { file }) => {
                let body = helpers::read_rules_file(&file)?;
                output::print_rules_applied(&c.rules_set(body).await?);
            }
            Some(RulesCommand::Remove { index }) => {
                output::print_rules_applied(&c.rules_remove(index).await?);
            }
        },
        Command::Approvals => output::print_approvals(&c.pending_approvals().await?),
        Command::Approve { action_id } => {
            output::print_approve(&c.approve(&action_id).await?);
        }
        Command::Deny { action_id } => {
            output::print_deny(&c.deny(&action_id).await?);
        }
        Command::DumpCheckpoint { seq, format } => {
            eprintln!("dump-checkpoint not yet implemented (seq={seq}, format={format})");
        }
        Command::Agents { bucket: _ } => {
            output::print_agents(&c.agents().await?);
        }
        Command::Timeline { agents, since, event_type } => {
            let params = helpers::build_timeline_params(&agents, since.as_deref(), event_type.as_deref());
            let body = c.timeline(&params).await?;
            print!("{body}");
        }
        Command::Correlate { write_agent, read_agent, resource } => {
            output::print_correlations(
                &c.correlate(&write_agent, &read_agent, resource.as_deref()).await?,
            );
        }
    }

    Ok(())
}
