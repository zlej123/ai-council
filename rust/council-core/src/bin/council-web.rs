use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use council_core::engine::AgentDisposition;
use council_core::providers::{
    AgentSpec, ProviderKind, UsageSample, build_adapters_with, check_subscription,
};
use council_core::transcript::{barrier_line, render_session_markdown};
use council_core::{AgentId, Author, Council, CycleOutcome, Progress, RoomEvent};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock, mpsc};

const INDEX_HTML: &str = include_str!("../../web/index.html");

#[derive(Clone, Serialize, Deserialize)]
struct UiEvent {
    id: u64,
    author: String,
    content: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct UiBarrier {
    line: String,
    #[serde(default)]
    reasons: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct UiCycle {
    barriers: Vec<UiBarrier>,
    stop: String,
}

#[derive(Clone, Default, Serialize)]
struct UsageTotals {
    calls: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
}

#[derive(Clone, Copy, Default, Serialize)]
struct Budget {
    max_cost_usd: f64,
    max_total_tokens: u64,
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
    progress: BTreeMap<String, String>,
    budget: Budget,
    models: BTreeMap<String, String>,
}

/// Sidecar JSON written next to each Markdown transcript so a room can be
/// listed, viewed, and resumed later.
#[derive(Serialize, Deserialize)]
struct SessionRecord {
    saved_unix: u64,
    provider: String,
    roster: Vec<String>,
    events: Vec<UiEvent>,
    cycles: Vec<UiCycle>,
    metrics: serde_json::Value,
}

enum Command {
    Message {
        text: String,
        target: Option<AgentId>,
    },
    Rate(u8, Option<String>),
    Reset {
        seats: Vec<AgentSpec>,
        max_ai_streak: u64,
    },
    Resume {
        file: String,
    },
}

struct App {
    ui: RwLock<UiState>,
    commands: mpsc::Sender<Command>,
    stop: Notify,
    outputs_dir: PathBuf,
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
    target: Option<String>,
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

#[derive(Deserialize)]
struct BudgetBody {
    max_cost_usd: Option<f64>,
    max_total_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct FileBody {
    file: String,
}

#[derive(Serialize)]
struct ProviderStatus {
    agent: String,
    ok: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct SessionListing {
    file: String,
    saved_unix: u64,
    events: usize,
    first_line: String,
}

fn seat_models(council: &Council) -> BTreeMap<String, String> {
    council
        .seats()
        .into_iter()
        .map(|(agent, model)| {
            (
                agent.to_string(),
                model.unwrap_or_else(|| "기본".to_owned()),
            )
        })
        .collect()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn disposition_label(disposition: AgentDisposition) -> &'static str {
    match disposition {
        AgentDisposition::Pass => "PASS",
        AgentDisposition::RequestFloor => "REQUEST",
        AgentDisposition::SyncOnly => "동기화",
    }
}

fn ui_cycle(outcome: &CycleOutcome) -> UiCycle {
    UiCycle {
        barriers: outcome
            .barriers
            .iter()
            .map(|barrier| UiBarrier {
                line: barrier_line(barrier),
                reasons: barrier
                    .reasons
                    .iter()
                    .map(|(agent, reason)| format!("{agent}: {reason}"))
                    .collect(),
            })
            .collect(),
        stop: outcome.stop_reason.to_string(),
    }
}

fn parse_author(name: &str) -> Option<Author> {
    if name == "You" {
        return Some(Author::You);
    }
    AgentId::parse(&name.to_ascii_lowercase()).map(Author::Agent)
}

/// Only bare transcript basenames are ever accepted from the client.
fn safe_session_file(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name.ends_with(".json")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_');
    ok.then(|| name.to_owned())
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
    let mut session_base = format!("web-session-{}", now_unix());

    let ui = UiState {
        provider: provider.label().to_owned(),
        roster: council.roster().iter().map(ToString::to_string).collect(),
        events: Vec::new(),
        cycles: Vec::new(),
        metrics: serde_json::to_value(council.metrics_report())?,
        busy: false,
        last_error: None,
        transcript: format!("{session_base}.md"),
        usage: BTreeMap::new(),
        max_ai_streak,
        progress: BTreeMap::new(),
        budget: Budget::default(),
        models: seat_models(&council),
    };

    let (command_tx, mut command_rx) = mpsc::channel::<Command>(16);
    let app = Arc::new(App {
        ui: RwLock::new(ui),
        commands: command_tx,
        stop: Notify::new(),
        outputs_dir: outputs_dir.clone(),
    });

    // Committed events stream into the UI state as they happen, so the page
    // shows each speech mid-cycle instead of waiting for the whole cycle.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    council.set_event_sink(event_tx.clone());
    let event_app = Arc::clone(&app);
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let mut ui = event_app.ui.write().await;
            let author = event.author.to_string();
            ui.progress.remove(&author);
            ui.events.push(UiEvent {
                id: event.id,
                author,
                content: event.content,
            });
        }
    });

    // Mid-cycle deliberation status per agent.
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<Progress>();
    council.set_progress_sink(progress_tx.clone());
    let progress_app = Arc::clone(&app);
    tokio::spawn(async move {
        while let Some(update) = progress_rx.recv().await {
            let mut ui = progress_app.ui.write().await;
            match update {
                Progress::BarrierStarted { .. } => {
                    let roster = ui.roster.clone();
                    for agent in roster {
                        ui.progress.insert(agent, "생각 중".to_owned());
                    }
                }
                Progress::Decided { agent, disposition } => {
                    ui.progress
                        .insert(agent.to_string(), disposition_label(disposition).to_owned());
                }
                Progress::Speaking { agent } => {
                    ui.progress
                        .insert(agent.to_string(), "발언 작성 중".to_owned());
                }
            }
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
        let mut current_seats = seats;
        let mut current_streak = max_ai_streak;

        while let Some(command) = command_rx.recv().await {
            match command {
                Command::Message { text, target } => {
                    let result = tokio::select! {
                        result = council.submit_human_directed(text, target) => Some(result),
                        _ = council_app.stop.notified() => None,
                    };
                    let mut ui = council_app.ui.write().await;
                    match result {
                        Some(Ok(outcome)) => {
                            ui.cycles.push(ui_cycle(&outcome));
                            ui.last_error = None;
                            cycles.push(outcome);
                        }
                        Some(Err(error)) => ui.last_error = Some(error.to_string()),
                        None => {
                            ui.last_error =
                                Some("심의를 중단했다. 이미 커밋된 발언은 유지된다.".to_owned());
                        }
                    }
                    ui.metrics = serde_json::to_value(council.metrics_report())
                        .unwrap_or(serde_json::Value::Null);
                    ui.progress.clear();
                    ui.busy = false;
                    drop(ui);
                    save_session(
                        &council_app,
                        provider_label,
                        &council,
                        &cycles,
                        &session_base,
                    )
                    .await;
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
                    drop(ui);
                    save_session(
                        &council_app,
                        provider_label,
                        &council,
                        &cycles,
                        &session_base,
                    )
                    .await;
                }
                Command::Reset {
                    seats: next_seats,
                    max_ai_streak,
                } => {
                    let rebuilt = build_adapters_with(provider, &next_seats, Some(&usage_tx))
                        .and_then(|adapters| Council::new(adapters, max_ai_streak));
                    let mut ui = council_app.ui.write().await;
                    match rebuilt {
                        Ok(mut next) => {
                            next.set_event_sink(event_tx.clone());
                            next.set_progress_sink(progress_tx.clone());
                            council = next;
                            cycles.clear();
                            current_seats = next_seats;
                            current_streak = max_ai_streak;
                            session_base = format!("web-session-{}", now_unix());
                            ui.roster = council.roster().iter().map(ToString::to_string).collect();
                            ui.models = seat_models(&council);
                            ui.events.clear();
                            ui.cycles.clear();
                            ui.usage.clear();
                            ui.progress.clear();
                            ui.metrics = serde_json::to_value(council.metrics_report())
                                .unwrap_or(serde_json::Value::Null);
                            ui.transcript = format!("{session_base}.md");
                            ui.max_ai_streak = max_ai_streak;
                            ui.last_error = None;
                        }
                        Err(error) => ui.last_error = Some(error.to_string()),
                    }
                    ui.busy = false;
                }
                Command::Resume { file } => {
                    let resumed = resume_council(
                        &council_app.outputs_dir,
                        &file,
                        provider,
                        &current_seats,
                        current_streak,
                        &usage_tx,
                        &event_tx,
                        &progress_tx,
                    );
                    let mut ui = council_app.ui.write().await;
                    match resumed {
                        Ok(next) => {
                            // The seeded events arrive through the event sink;
                            // clear the old view first so they land in order.
                            ui.events.clear();
                            ui.cycles.clear();
                            ui.usage.clear();
                            ui.progress.clear();
                            council = next;
                            cycles.clear();
                            session_base = format!("web-session-{}", now_unix());
                            ui.roster = council.roster().iter().map(ToString::to_string).collect();
                            ui.models = seat_models(&council);
                            ui.metrics = serde_json::to_value(council.metrics_report())
                                .unwrap_or(serde_json::Value::Null);
                            ui.transcript = format!("{session_base}.md");
                            ui.last_error = None;
                        }
                        Err(error) => ui.last_error = Some(error),
                    }
                    ui.busy = false;
                }
            }
        }
    });

    let router = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/state", get(get_state))
        .route("/api/message", post(post_message))
        .route("/api/stop", post(post_stop))
        .route("/api/rate", post(post_rate))
        .route("/api/providers", get(get_providers))
        .route("/api/session", post(post_session))
        .route("/api/budget", post(post_budget))
        .route("/api/sessions", get(get_sessions))
        .route("/api/session_view", get(get_session_view))
        .route("/api/resume", post(post_resume))
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("AI Council web UI on http://127.0.0.1:{port}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn save_session(
    app: &Arc<App>,
    provider_label: &str,
    council: &Council,
    cycles: &[CycleOutcome],
    session_base: &str,
) {
    if cycles.is_empty() {
        return;
    }
    let snapshot = council.room().snapshot();
    let metrics = council.metrics_report();
    let markdown = render_session_markdown(provider_label, &snapshot, cycles, &metrics);
    let record = SessionRecord {
        saved_unix: now_unix(),
        provider: provider_label.to_owned(),
        roster: council.roster().iter().map(ToString::to_string).collect(),
        events: snapshot
            .events
            .iter()
            .map(|event| UiEvent {
                id: event.id,
                author: event.author.to_string(),
                content: event.content.clone(),
            })
            .collect(),
        cycles: cycles.iter().map(ui_cycle).collect(),
        metrics: serde_json::to_value(&metrics).unwrap_or(serde_json::Value::Null),
    };
    let dir = &app.outputs_dir;
    let _ = std::fs::write(dir.join(format!("{session_base}.md")), markdown);
    if let Ok(json) = serde_json::to_string_pretty(&record) {
        let _ = std::fs::write(dir.join(format!("{session_base}.json")), json);
    }
}

#[allow(clippy::too_many_arguments)]
fn resume_council(
    outputs_dir: &std::path::Path,
    file: &str,
    provider: ProviderKind,
    seats: &[AgentSpec],
    max_ai_streak: u64,
    usage_tx: &mpsc::UnboundedSender<UsageSample>,
    event_tx: &mpsc::UnboundedSender<RoomEvent>,
    progress_tx: &mpsc::UnboundedSender<Progress>,
) -> Result<Council, String> {
    let file = safe_session_file(file).ok_or("잘못된 세션 파일 이름")?;
    let raw = std::fs::read_to_string(outputs_dir.join(&file))
        .map_err(|error| format!("세션 파일을 읽지 못했다: {error}"))?;
    let record: SessionRecord =
        serde_json::from_str(&raw).map_err(|error| format!("세션 파일 형식 오류: {error}"))?;
    let mut events = Vec::new();
    for event in record.events {
        let author =
            parse_author(&event.author).ok_or(format!("알 수 없는 화자: {}", event.author))?;
        events.push(RoomEvent {
            id: event.id,
            author,
            content: event.content,
        });
    }
    let adapters =
        build_adapters_with(provider, seats, Some(usage_tx)).map_err(|error| error.to_string())?;
    let mut council = Council::new(adapters, max_ai_streak).map_err(|error| error.to_string())?;
    council.set_event_sink(event_tx.clone());
    council.set_progress_sink(progress_tx.clone());
    council
        .seed_room(events)
        .map_err(|error| error.to_string())?;
    Ok(council)
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
    let target = match body.target.as_deref().filter(|value| !value.is_empty()) {
        Some(name) => Some(
            AgentId::parse(name)
                .ok_or((StatusCode::BAD_REQUEST, format!("unknown agent: {name}")))?,
        ),
        None => None,
    };
    {
        let mut ui = app.ui.write().await;
        if ui.busy {
            return Err((
                StatusCode::CONFLICT,
                "the council is still deliberating".to_owned(),
            ));
        }
        let spent_cost: f64 = ui.usage.values().map(|u| u.cost_usd).sum();
        let spent_tokens: u64 = ui
            .usage
            .values()
            .map(|u| u.input_tokens + u.output_tokens)
            .sum();
        if ui.budget.max_cost_usd > 0.0 && spent_cost >= ui.budget.max_cost_usd {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "세션 비용 예산(${:.2})에 도달했다. 설정에서 예산을 올리거나 새 세션을 시작하라.",
                    ui.budget.max_cost_usd
                ),
            ));
        }
        if ui.budget.max_total_tokens > 0 && spent_tokens >= ui.budget.max_total_tokens {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "세션 토큰 예산({})에 도달했다. 설정에서 예산을 올리거나 새 세션을 시작하라.",
                    ui.budget.max_total_tokens
                ),
            ));
        }
        ui.busy = true;
    }
    app.commands
        .send(Command::Message {
            text: body.text,
            target,
        })
        .await
        .map_err(task_stopped)?;
    Ok(StatusCode::ACCEPTED)
}

async fn post_stop(State(app): State<Arc<App>>) -> StatusCode {
    app.stop.notify_waiters();
    StatusCode::ACCEPTED
}

async fn post_rate(
    State(app): State<Arc<App>>,
    Json(body): Json<RateBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    app.commands
        .send(Command::Rate(body.score, body.note))
        .await
        .map_err(task_stopped)?;
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
    mark_busy_for_command(&app).await?;
    app.commands
        .send(Command::Reset {
            seats,
            max_ai_streak: body.max_ai_streak.unwrap_or(3),
        })
        .await
        .map_err(task_stopped)?;
    Ok(StatusCode::ACCEPTED)
}

async fn post_budget(State(app): State<Arc<App>>, Json(body): Json<BudgetBody>) -> StatusCode {
    let mut ui = app.ui.write().await;
    if let Some(cost) = body.max_cost_usd {
        ui.budget.max_cost_usd = cost.max(0.0);
    }
    if let Some(tokens) = body.max_total_tokens {
        ui.budget.max_total_tokens = tokens;
    }
    StatusCode::ACCEPTED
}

async fn get_sessions(State(app): State<Arc<App>>) -> Json<Vec<SessionListing>> {
    let mut listings = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&app.outputs_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<SessionRecord>(&raw) else {
                continue;
            };
            let first_line = record
                .events
                .first()
                .map(|event| event.content.chars().take(80).collect())
                .unwrap_or_default();
            listings.push(SessionListing {
                file: name,
                saved_unix: record.saved_unix,
                events: record.events.len(),
                first_line,
            });
        }
    }
    listings.sort_by_key(|listing| std::cmp::Reverse(listing.saved_unix));
    Json(listings)
}

#[derive(Deserialize)]
struct ViewQuery {
    file: String,
}

async fn get_session_view(
    State(app): State<Arc<App>>,
    Query(query): Query<ViewQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let file = safe_session_file(&query.file)
        .ok_or((StatusCode::BAD_REQUEST, "invalid file name".to_owned()))?;
    let raw = std::fs::read_to_string(app.outputs_dir.join(file))
        .map_err(|error| (StatusCode::NOT_FOUND, error.to_string()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(value))
}

async fn post_resume(
    State(app): State<Arc<App>>,
    Json(body): Json<FileBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    safe_session_file(&body.file)
        .ok_or((StatusCode::BAD_REQUEST, "invalid file name".to_owned()))?;
    mark_busy_for_command(&app).await?;
    app.commands
        .send(Command::Resume { file: body.file })
        .await
        .map_err(task_stopped)?;
    Ok(StatusCode::ACCEPTED)
}

async fn mark_busy_for_command(app: &Arc<App>) -> Result<(), (StatusCode, String)> {
    let mut ui = app.ui.write().await;
    if ui.busy {
        return Err((
            StatusCode::CONFLICT,
            "the council is still deliberating".to_owned(),
        ));
    }
    ui.busy = true;
    Ok(())
}

fn task_stopped<T>(_: T) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "council task stopped".to_owned(),
    )
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
