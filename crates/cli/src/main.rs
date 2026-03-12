use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use sandbox::cas::ContentHash;
use sandbox::events::envelope::{Event, EventPayload};
use sandbox::events::io::StdioSubtype;

#[derive(Parser)]
#[command(name = "argus", about = "Argus sandbox CLI")]
struct Cli {
    /// CAS directory (default: /data/cas)
    #[arg(long, default_value = "/data/cas", global = true)]
    cas_dir: PathBuf,

    /// Event log directory (default: /data/events)
    #[arg(long, default_value = "/data/events", global = true)]
    event_dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show event log entries
    Log {
        /// Filter by path (exact match)
        #[arg(long)]
        path: Option<String>,

        /// Filter by PID
        #[arg(long)]
        pid: Option<u32>,

        /// Filter by event type (e.g. write, read, exec)
        #[arg(long, name = "type")]
        event_type: Option<String>,

        /// Filter events after this wall-clock timestamp (RFC 3339)
        #[arg(long)]
        since: Option<String>,

        /// Maximum number of events to display
        #[arg(long, default_value = "1000")]
        limit: usize,

        /// Output raw JSON lines instead of human-readable format
        #[arg(long)]
        json: bool,
    },

    /// Print CAS object content to stdout
    Cat {
        /// Content hash (64-char hex SHA-256)
        hash: String,
    },

    /// Reconstruct stdio output for a process
    Stdio {
        /// Process ID
        pid: u32,

        /// Stream to show: stdout, stderr, or all (default: all)
        #[arg(long, default_value = "all")]
        stream: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Log {
            path,
            pid,
            event_type,
            since,
            limit,
            json,
        } => cmd_log(
            &cli.event_dir,
            path.as_deref(),
            pid,
            event_type.as_deref(),
            since.as_deref(),
            limit,
            json,
        ),
        Command::Cat { hash } => cmd_cat(&cli.cas_dir, &hash),
        Command::Stdio { pid, stream } => {
            cmd_stdio(&cli.event_dir, &cli.cas_dir, pid, &stream)
        }
    }
}

fn cmd_log(
    event_dir: &PathBuf,
    path: Option<&str>,
    pid: Option<u32>,
    event_type: Option<&str>,
    since: Option<&str>,
    limit: usize,
    json_output: bool,
) -> Result<()> {
    let events = read_all_events(event_dir)?;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut count = 0;

    for event in &events {
        if count >= limit {
            break;
        }

        if let Some(filter_pid) = pid {
            if event.payload.pid() != Some(filter_pid) {
                continue;
            }
        }

        if let Some(filter_type) = event_type {
            if event.payload.event_type_tag() != filter_type {
                continue;
            }
        }

        if let Some(filter_path) = path {
            let paths = event.payload.paths();
            if !paths.iter().any(|p| *p == filter_path) {
                continue;
            }
        }

        if let Some(since_ts) = since {
            if event.ts_wall.as_str() < since_ts {
                continue;
            }
        }

        if json_output {
            serde_json::to_writer(&mut out, event)
                .context("serialize event")?;
            writeln!(out)?;
        } else {
            write_event_human(&mut out, event)?;
        }

        count += 1;
    }

    Ok(())
}

fn cmd_cat(cas_dir: &PathBuf, hash_str: &str) -> Result<()> {
    let hash = ContentHash::try_from(hash_str.to_string())
        .map_err(|e| anyhow::anyhow!("invalid hash: {e}"))?;

    let path = cas_dir.join(hash.prefix()).join(hash.suffix());

    let data = fs::read(&path)
        .with_context(|| format!("read CAS object at {}", path.display()))?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(&data)?;

    Ok(())
}

fn cmd_stdio(
    event_dir: &PathBuf,
    cas_dir: &PathBuf,
    pid: u32,
    stream: &str,
) -> Result<()> {
    let events = read_all_events(event_dir)?;
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for event in &events {
        if let EventPayload::Stdio(stdio) = &event.payload {
            if stdio.pid != pid {
                continue;
            }

            let include = match stream {
                "stdout" => stdio.subtype == StdioSubtype::Stdout,
                "stderr" => stdio.subtype == StdioSubtype::Stderr,
                "stdin" => stdio.subtype == StdioSubtype::Stdin,
                "all" => true,
                other => bail!("unknown stream: {other} (use stdout, stderr, stdin, or all)"),
            };

            if !include {
                continue;
            }

            if let Some(hash_str) = &stdio.content_hash {
                let hash = ContentHash::try_from(hash_str.clone())
                    .map_err(|e| anyhow::anyhow!("invalid hash in event: {e}"))?;
                let path = cas_dir.join(hash.prefix()).join(hash.suffix());
                match fs::read(&path) {
                    Ok(data) => out.write_all(&data)?,
                    Err(e) => {
                        eprintln!(
                            "warning: CAS object {} not found: {e}",
                            hash_str
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Read and parse all JSONL event files from the event directory,
/// sorted by segment sequence number.
fn read_all_events(event_dir: &PathBuf) -> Result<Vec<Event>> {
    if !event_dir.exists() {
        bail!("event directory does not exist: {}", event_dir.display());
    }

    let mut segments: Vec<PathBuf> = fs::read_dir(event_dir)
        .with_context(|| format!("read event dir {}", event_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("jsonl")
        })
        .collect();

    segments.sort_by(|a, b| {
        let seq_a = segment_seq(a);
        let seq_b = segment_seq(b);
        seq_a.cmp(&seq_b)
    });

    let mut events = Vec::new();
    for path in &segments {
        let content = fs::read_to_string(path)
            .with_context(|| format!("read segment {}", path.display()))?;

        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(line)
                .with_context(|| {
                    format!(
                        "parse event at {}:{}",
                        path.display(),
                        line_num + 1,
                    )
                })?;
            events.push(event);
        }
    }

    Ok(events)
}

/// Extract segment sequence number from filename like "0.jsonl".
fn segment_seq(path: &PathBuf) -> u64 {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(u64::MAX)
}

fn write_event_human(out: &mut impl Write, event: &Event) -> Result<()> {
    let tag = event.payload.event_type_tag();
    let pid_str = event
        .payload
        .pid()
        .map(|p| format!(" pid={p}"))
        .unwrap_or_default();

    let paths = event.payload.paths();
    let path_str = if paths.is_empty() {
        String::new()
    } else {
        format!(" {}", paths.join(" → "))
    };

    writeln!(
        out,
        "[{seq}] {ts} {tag}{pid}{path}",
        seq = event.seq,
        ts = &event.ts_wall[..19],
        tag = tag,
        pid = pid_str,
        path = path_str,
    )?;

    Ok(())
}
