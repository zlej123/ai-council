use std::error::Error;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use council_core::providers::{ProviderKind, build_adapters};
use council_core::transcript::{barrier_line, render_session_markdown};
use council_core::{AgentId, Council, CycleOutcome};
use tokio::io::{self, AsyncBufReadExt, BufReader};

struct Options {
    provider: ProviderKind,
    agents: Vec<AgentId>,
    max_ai_streak: u64,
    once: Option<String>,
    check_providers: bool,
    transcript: Option<PathBuf>,
    no_transcript: bool,
}

impl Options {
    fn provider_label(&self) -> &'static str {
        self.provider.label()
    }

    fn transcript_path(&self) -> Option<PathBuf> {
        if self.no_transcript {
            return None;
        }
        Some(self.transcript.clone().unwrap_or_else(|| {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            PathBuf::from(format!("outputs/session-{seconds}.md"))
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options()?;
    let mut council = build_council(options.provider, &options.agents, options.max_ai_streak)?;

    if options.check_providers {
        println!("Provider authentication and executables are ready; no model was called.");
        return Ok(());
    }

    let transcript_path = options.transcript_path();
    let mut cycles: Vec<CycleOutcome> = Vec::new();

    if let Some(message) = options.once.clone() {
        let outcome = council.submit_human(message).await?;
        print_outcome(&outcome);
        cycles.push(outcome);
        print_metrics(&council)?;
        save_transcript(
            &options,
            &council,
            &cycles,
            transcript_path.as_deref(),
            true,
        );
        return Ok(());
    }

    println!("Council Core Spike ready.");
    println!(
        "Provider: {} · agents: {} · max AI streak: {}",
        options.provider_label(),
        council
            .roster()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        options.max_ai_streak
    );
    println!("Type a message, or /help for experimental commands.\n");

    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/quit" {
            break;
        }
        if input == "/help" {
            println!("/metrics · /state · /rate 1..5 optional note · /quit");
            continue;
        }
        if input == "/metrics" {
            print_metrics(&council)?;
            continue;
        }
        if input == "/state" {
            println!("{}", serde_json::to_string_pretty(council.agent_states())?);
            continue;
        }
        if let Some(arguments) = input.strip_prefix("/rate ") {
            handle_rating(&mut council, arguments);
            continue;
        }
        if input.starts_with('/') {
            eprintln!("Unknown command. Use /help.");
            continue;
        }

        match council.submit_human(input.to_owned()).await {
            Ok(outcome) => {
                print_outcome(&outcome);
                cycles.push(outcome);
                save_transcript(
                    &options,
                    &council,
                    &cycles,
                    transcript_path.as_deref(),
                    false,
                );
            }
            Err(error) => eprintln!("Council stopped before granting the floor: {error}"),
        }
    }

    println!("\nFinal in-memory metrics:");
    print_metrics(&council)?;
    save_transcript(
        &options,
        &council,
        &cycles,
        transcript_path.as_deref(),
        true,
    );
    Ok(())
}

fn save_transcript(
    options: &Options,
    council: &Council,
    cycles: &[CycleOutcome],
    path: Option<&std::path::Path>,
    announce: bool,
) {
    let Some(path) = path else {
        return;
    };
    if cycles.is_empty() {
        return;
    }
    let markdown = render_session_markdown(
        options.provider_label(),
        &council.room().snapshot(),
        cycles,
        &council.metrics_report(),
    );
    let written = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(path, markdown));
    match written {
        Ok(()) if announce => println!("Transcript saved to {}", path.display()),
        Ok(()) => {}
        Err(error) => eprintln!("Failed to save transcript to {}: {error}", path.display()),
    }
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut provider = ProviderKind::Mock;
    let mut agents = vec![AgentId::Gpt, AgentId::Claude];
    let mut max_ai_streak = 3;
    let mut once = None;
    let mut check_providers = false;
    let mut transcript = None;
    let mut no_transcript = false;
    let mut arguments = std::env::args().skip(1);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--provider" => {
                let value = arguments
                    .next()
                    .ok_or("--provider needs mock, subscription, or live")?;
                provider = ProviderKind::parse(&value)
                    .ok_or_else(|| format!("unknown provider mode: {value}"))?;
            }
            "--agents" => {
                let value = arguments
                    .next()
                    .ok_or("--agents needs a comma-separated list, e.g. gpt,claude,gemini,grok")?;
                agents = AgentId::parse_list(&value)?;
            }
            "--max-ai-streak" => {
                max_ai_streak = arguments
                    .next()
                    .ok_or("--max-ai-streak needs a non-negative integer")?
                    .parse()?;
            }
            "--once" => {
                once = Some(arguments.next().ok_or("--once needs a message")?);
            }
            "--check-providers" => check_providers = true,
            "--transcript" => {
                transcript = Some(PathBuf::from(
                    arguments.next().ok_or("--transcript needs a file path")?,
                ));
            }
            "--no-transcript" => no_transcript = true,
            "--help" | "-h" => {
                println!(
                    "Usage: council-core [--provider mock|subscription|live] [--agents gpt,claude,gemini,grok] [--max-ai-streak N] [--once MESSAGE] [--check-providers] [--transcript PATH] [--no-transcript]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }

    Ok(Options {
        provider,
        agents,
        max_ai_streak,
        once,
        check_providers,
        transcript,
        no_transcript,
    })
}

fn build_council(
    mode: ProviderKind,
    agents: &[AgentId],
    max_ai_streak: u64,
) -> Result<Council, Box<dyn Error>> {
    Ok(Council::new(build_adapters(mode, agents)?, max_ai_streak)?)
}

fn print_outcome(outcome: &CycleOutcome) {
    for event in &outcome.appended_events {
        if let council_core::Author::Agent(_) = event.author {
            println!("\n{}\n{}", event.author, event.content);
        }
    }
    println!("\n[control]");
    for barrier in &outcome.barriers {
        println!("{}", barrier_line(barrier));
    }
    println!("stop={}", outcome.stop_reason);
}

fn print_metrics(council: &Council) -> Result<(), serde_json::Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(&council.metrics_report())?
    );
    Ok(())
}

fn handle_rating(council: &mut Council, arguments: &str) {
    let mut parts = arguments.splitn(2, char::is_whitespace);
    let Some(score) = parts.next().and_then(|value| value.parse::<u8>().ok()) else {
        eprintln!("Usage: /rate 1..5 optional note");
        return;
    };
    let note = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    match council.rate_naturalness(score, note) {
        Ok(()) => println!("Naturalness rating recorded in memory."),
        Err(error) => eprintln!("{error}"),
    }
}
