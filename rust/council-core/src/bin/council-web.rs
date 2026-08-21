use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use council_core::providers::{
    AgentSpec, ProviderKind, UsageSample, build_adapters_with, check_subscription,
};
use council_core::transcript::{barrier_line, render_session_markdown};
use council_core::{AgentId, Council, CycleOutcome};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use tokio::sync::{RwLock, mpsc};

const INDEX_HTML: &str = include_str!("../../web/index.html");

#[derive(Clone, Serialize)]
struct UiEvent {
    id: u64,
    author: String,
    content: String,
}

#[derive(Clone, Serialize)]
struct UiCycle {
    barriers: Vec<String>,
    stop: String,
}

#[derive(Clone, Default, Serialize)]
struct UsageTotals {
    calls: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
}

#[derive(Clone, Serialize)]
struct UiState {
    provider: String,
    roster: Vec<String>,
    events: Vec<UiEvent>,
    cycles: Vec<UiCycle>,
    metrics: serde_json::Value,
    busy: bool,
    last_error: Option<String>,
    transcript: String,
    usage: BTreeMap<String, UsageTotals>,
    max_ai_streak: u64,
}

enum Command {
    Message(String),
    Rate(u8, Option<String>),
    Reset {
        seats: Vec<AgentSpec>,
        max_ai_streak: u64,
    },
}

struct App {
    ui: RwLock<UiState>,
    commands: mpsc::Sender<Command>,
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
}

#[derive(Deserialize)]
struct RateBody {
    score: u8,
    note: Option<String>,
}

#[derive(Deserialize)]
struct SeatBody {
    agent: String,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
struct SessionBody {
    seats: Vec<SeatBody>,
    max_ai_streak: Option<u64>,
}

#[derive(Serialize)]
struct ProviderStatus {
    agent: String,
    ok: bool,
    error: Option<String>,
}

fn transcript_file() -> PathBuf {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    PathBuf::from(format!("web-session-{seconds}.md"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (provider, agents, max_ai_streak, port) = parse_options()?;
    let (usage_tx, mut usage_rx) = mpsc::unbounded_channel::<UsageSample>();
    let seats: Vec<AgentSpec> = agents.iter().copied().map(AgentSpec::defaults).collect();
    let mut council = Council::new(
        build_adapters_with(provider, &seats, Some(&usage_tx))?,
        max_ai_streak,
    )?;

    let outputs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../outputs");
    std::fs::create_dir_all(&outputs_dir)?;
    let outputs_dir = outputs_dir.canonicalize()?;
    let mut transcript_path = outputs_dir.join(transcript_file());

    let ui = UiState {
        provider: provider.label().to_owned(),
        roster: council.roster().iter().map(ToString::to_string).collect(),
        events: Vec::new(),
        cycles: Vec::new(),
        metrics: serde_json::to_value(council.metrics_report())?,
        busy: false,
        last_error: None,
        transcript: transcript_path.display().to_string(),
        usage: BTreeMap::new(),
        max_ai_streak,
    };

    let (command_tx, mut command_rx) = mpsc::channel::<Command>(16);
    let app = Arc::new(App {
        ui: RwLock::new(ui),
        commands: command_tx,
    });

    // Committed events stream into the UI state as they happen, so the page
    // shows each speech mid-cycle instead of waiting for the whole cycle.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    council.set_event_sink(event_tx.clone());
    let event_app = Arc::clone(&app);
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            event_app.ui.write().await.events.push(UiEvent {
                id: event.id,
                author: event.author.to_string(),
                content: event.content,
            });
        }
    });

    // Per-seat session usage totals, observed from each CLI response.
    let usage_app = Arc::clone(&app);
    tokio::spawn(async move {
        while let Some(sample) = usage_rx.recv().await {
            let mut ui = usage_app.ui.write().await;
            let totals = ui.usage.entry(sample.agent.to_string()).or_default();
            totals.calls += 1;
            totals.input_tokens += sample.input_tokens;
            totals.output_tokens += sample.output_tokens;
            totals.cost_usd += sample.cost_usd;
        }
    });

    let council_app = Arc::clone(&app);
    let provider_label = provider.label();
    tokio::spawn(async move {
        let mut cycles: Vec<CycleOutcome> = Vec::new();
        while let Some(command) = command_rx.recv().await {
            match command {
                Command::Reset {
                    seats,
                    max_ai_streak,
                } => {
                    let rebuilt = build_adapters_with(provider, &seats, Some(&usage_tx))
                        .and_then(|adapters| Council::new(adapters, max_ai_streak));
                    let mut ui = council_app.ui.write().await;
                    match rebuilt {
                        Ok(mut next) => {
                            next.set_event_sink(event_tx.clone());
                            council = next;
                            cycles.clear();
                            transcript_path = outputs_dir.join(transcript_file());
                            ui.roster = council.roster().iter().map(ToString::to_string).collect();
                            ui.events.clear();
                            ui.cycles.clear();
                            ui.usage.clear();
                            ui.metrics = serde_json::to_value(council.metrics_report())
                                .unwrap_or(serde_json::Value::Null);
                            ui.transcript = transcript_path.display().to_string();
                            ui.max_ai_streak = max_ai_streak;
                            ui.last_error = None;
                        }
                        Err(error) => ui.last_error = Some(error.to_string()),
                    }
                    ui.busy = false;
                    continue;
                }
                Command::Message(text) => {
                    let result = council.submit_human(text).await;
                    let mut ui = council_app.ui.write().await;
                    match result {
                        Ok(outcome) => {
                            ui.cycles.push(UiCycle {
                                barriers: outcome.barriers.iter().map(barrier_line).collect(),
                                stop: outcome.stop_reason.to_string(),
                            });
                            ui.last_error = None;
                            cycles.push(outcome);
                        }
                        Err(error) => ui.last_error = Some(error.to_string()),
                    }
                    ui.metrics = serde_json::to_value(council.metrics_report())
                        .unwrap_or(serde_json::Value::Null);
                    ui.busy = false;
                }
                Command::Rate(score, note) => {
                    let outcome = council.rate_naturalness(score, note);
                    let mut ui = council_app.ui.write().await;
                    match outcome {
                        Ok(()) => {
                            ui.metrics = serde_json::to_value(council.metrics_report())
                                .unwrap_or(serde_json::Value::Null);
                            ui.last_error = None;
                        }
                        Err(error) => ui.last_error = Some(error.to_owned()),
                    }
                }
            }
            if !cycles.is_empty() {
                let markdown = render_session_markdown(
                    provider_label,
                    &council.room().snapshot(),
                    &cycles,
                    &council.metrics_report(),
                );
                if let Some(parent) = transcript_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&transcript_path, markdown);
            }
        }
    });

    let router = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/state", get(get_state))
        .route("/api/message", post(post_message))
        .route("/api/rate", post(post_rate))
        .route("/api/providers", get(get_providers))
        .route("/api/session", post(post_session))
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Council web UI on http://127.0.0.1:{port}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn get_state(State(app): State<Arc<App>>) -> Json<UiState> {
    Json(app.ui.read().await.clone())
}

async fn post_message(
    State(app): State<Arc<App>>,
    Json(body): Json<MessageBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    if body.text.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty message".to_owned()));
    }
    {
        let mut ui = app.ui.write().await;
        if ui.busy {
            return Err((
                StatusCode::CONFLICT,
                "the council is still deliberating".to_owned(),
            ));
        }
        ui.busy = true;
    }
    app.commands
        .send(Command::Message(body.text))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "council task stopped".to_owned(),
            )
        })?;
    Ok(StatusCode::ACCEPTED)
}

async fn post_rate(
    State(app): State<Arc<App>>,
    Json(body): Json<RateBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    app.commands
        .send(Command::Rate(body.score, body.note))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "council task stopped".to_owned(),
            )
        })?;
    Ok(StatusCode::ACCEPTED)
}

async fn get_providers() -> Json<Vec<ProviderStatus>> {
    let checks = AgentId::ORDER.map(|agent| {
        tokio::task::spawn_blocking(move || match check_subscription(agent) {
            Ok(()) => ProviderStatus {
                agent: agent.to_string(),
                ok: true,
                error: None,
            },
            Err(error) => ProviderStatus {
                agent: agent.to_string(),
                ok: false,
                error: Some(error.to_string()),
            },
        })
    });
    let mut statuses = Vec::new();
    for check in checks {
        if let Ok(status) = check.await {
            statuses.push(status);
        }
    }
    Json(statuses)
}

async fn post_session(
    State(app): State<Arc<App>>,
    Json(body): Json<SessionBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut seats = Vec::new();
    for seat in body.seats {
        let agent = AgentId::parse(&seat.agent).ok_or((
            StatusCode::BAD_REQUEST,
            format!("unknown agent: {}", seat.agent),
        ))?;
        seats.push(AgentSpec {
            agent,
            model: seat.model.filter(|value| !value.trim().is_empty()),
            effort: seat.effort.filter(|value| !value.trim().is_empty()),
        });
    }
    if seats.len() < 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            "a council needs at least two AI participants".to_owned(),
        ));
    }
    {
        let mut ui = app.ui.write().await;
        if ui.busy {
            return Err((
                StatusCode::CONFLICT,
                "the council is still deliberating".to_owned(),
            ));
        }
        ui.busy = true;
    }
    app.commands
        .send(Command::Reset {
            seats,
            max_ai_streak: body.max_ai_streak.unwrap_or(3),
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "council task stopped".to_owned(),
            )
        })?;
    Ok(StatusCode::ACCEPTED)
}

type Options = (ProviderKind, Vec<AgentId>, u64, u16);

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut provider = ProviderKind::Subscription;
    let mut agents = vec![AgentId::Claude, AgentId::Gemini, AgentId::Grok];
    let mut max_ai_streak = 3;
    let mut port = 8787;
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
                agents = value
                    .split(',')
                    .map(|name| {
                        AgentId::parse(name).ok_or_else(|| format!("unknown agent: {name}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            }
            "--max-ai-streak" => {
                max_ai_streak = arguments
                    .next()
                    .ok_or("--max-ai-streak needs a non-negative integer")?
                    .parse()?;
            }
            "--port" => {
                port = arguments.next().ok_or("--port needs a number")?.parse()?;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: council-web [--provider mock|subscription|live] [--agents claude,gemini,grok] [--max-ai-streak N] [--port 8787]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }

    Ok((provider, agents, max_ai_streak, port))
}
