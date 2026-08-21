use std::error::Error;
use std::sync::Arc;

use council_core::providers::{
    AnthropicAdapter, ClaudeCliAdapter, CodexCliAdapter, MockAdapter, OpenAiAdapter,
};
use council_core::{AgentAdapter, AgentId, Council, CycleOutcome};
use tokio::io::{self, AsyncBufReadExt, BufReader};

#[derive(Clone, Copy)]
enum ProviderMode {
    Mock,
    Subscription,
    Live,
}

struct Options {
    provider: ProviderMode,
    max_ai_streak: u64,
    once: Option<String>,
    check_providers: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options()?;
    let mut council = build_council(options.provider, options.max_ai_streak)?;

    if options.check_providers {
        println!("Provider authentication and executables are ready; no model was called.");
        return Ok(());
    }

    if let Some(message) = options.once {
        let outcome = council.submit_human(message).await?;
        print_outcome(&outcome);
        print_metrics(&council)?;
        return Ok(());
    }

    println!("Council Core Spike ready.");
    println!(
        "Provider: {} · max AI streak: {}",
        match options.provider {
            ProviderMode::Mock => "mock",
            ProviderMode::Subscription => "ChatGPT + Claude subscription CLIs",
            ProviderMode::Live => "live GPT + Claude",
        },
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
            Ok(outcome) => print_outcome(&outcome),
            Err(error) => eprintln!("Council stopped before granting the floor: {error}"),
        }
    }

    println!("\nFinal in-memory metrics:");
    print_metrics(&council)?;
    Ok(())
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut provider = ProviderMode::Mock;
    let mut max_ai_streak = 3;
    let mut once = None;
    let mut check_providers = false;
    let mut arguments = std::env::args().skip(1);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--provider" => {
                let value = arguments
                    .next()
                    .ok_or("--provider needs mock, subscription, or live")?;
                provider = match value.as_str() {
                    "mock" => ProviderMode::Mock,
                    "subscription" => ProviderMode::Subscription,
                    "live" => ProviderMode::Live,
                    _ => return Err(format!("unknown provider mode: {value}").into()),
                };
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
            "--help" | "-h" => {
                println!(
                    "Usage: council-core [--provider mock|subscription|live] [--max-ai-streak N] [--once MESSAGE] [--check-providers]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }

    Ok(Options {
        provider,
        max_ai_streak,
        once,
        check_providers,
    })
}

fn build_council(mode: ProviderMode, max_ai_streak: u64) -> Result<Council, Box<dyn Error>> {
    let adapters: Vec<Arc<dyn AgentAdapter>> = match mode {
        ProviderMode::Mock => vec![
            Arc::new(MockAdapter::new(AgentId::Gpt)),
            Arc::new(MockAdapter::new(AgentId::Claude)),
        ],
        ProviderMode::Subscription => vec![
            Arc::new(CodexCliAdapter::subscription()?),
            Arc::new(ClaudeCliAdapter::subscription()?),
        ],
        ProviderMode::Live => vec![
            Arc::new(OpenAiAdapter::from_env()?),
            Arc::new(AnthropicAdapter::from_env()?),
        ],
    };
    Ok(Council::new(adapters, max_ai_streak)?)
}

fn print_outcome(outcome: &CycleOutcome) {
    for event in &outcome.appended_events {
        if let council_core::Author::Agent(_) = event.author {
            println!("\n{}\n{}", event.author, event.content);
        }
    }
    println!("\n[control]");
    for barrier in &outcome.barriers {
        let details = barrier
            .dispositions
            .iter()
            .map(|(agent, disposition)| format!("{agent}={disposition:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let floor = barrier
            .floor_granted
            .map(|agent| agent.to_string())
            .unwrap_or_else(|| "none".to_owned());
        println!(
            "event #{}: {} · floor={} ",
            barrier.event_id, details, floor
        );
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
