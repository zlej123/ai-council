use std::path::PathBuf;
use std::process::{Command as SyncCommand, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::adapter::{AgentAdapter, CouncilError, CouncilResult};
use crate::model::{AgentId, Intent, RoomEvent, RoomSnapshot};
use crate::prompts::{SeatTools, evaluation_input, evaluation_instructions, speaking_input};

use super::{
    ProviderKind, SeatEnvironment, UsageSample, parse_decision, seat_tools, speak_instructions,
};

type UsageSink = tokio::sync::mpsc::UnboundedSender<UsageSample>;

/// The CLI executable for one seat, overridable per seat via env.
pub fn cli_binary(agent: AgentId) -> String {
    let (variable, default) = match agent {
        AgentId::Gpt => ("CODEX_CLI_BIN", "codex"),
        AgentId::Claude => ("CLAUDE_CLI_BIN", "claude"),
        AgentId::Gemini => ("ANTIGRAVITY_CLI_BIN", "agy"),
        AgentId::Grok => ("GROK_CLI_BIN", "grok"),
    };
    std::env::var(variable).unwrap_or_else(|_| default.to_owned())
}

fn report_usage(sink: &Option<UsageSink>, agent: AgentId, value: &Value) {
    let Some(sink) = sink else { return };
    let usage = value.get("usage").unwrap_or(&Value::Null);
    let read = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    // Claude and Grok report cache reads/creations on top of input_tokens
    // (additive); Codex `cached_input_tokens` and Antigravity
    // `cache_read_tokens` are subsets of input_tokens and must not be added.
    let _ = sink.send(UsageSample {
        agent,
        input_tokens: read("input_tokens")
            + read("cache_read_input_tokens")
            + read("cache_creation_input_tokens"),
        output_tokens: read("output_tokens"),
        cost_usd: value
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    });
}

const PROCESS_TIMEOUT: Duration = Duration::from_secs(180);
/// A speaking turn that may search the web or run requested work needs more
/// room than a judgement; EXPERIMENT.md §7 accepts minutes-long work turns.
const TOOL_TURN_TIMEOUT: Duration = Duration::from_secs(420);
const INTENT_SCHEMA: &str = include_str!("../../../../src/intent.schema.json");

pub struct CodexCliAdapter {
    usage_sink: Option<UsageSink>,
    environment: SeatEnvironment,
    binary: String,
    model: Option<String>,
    effort: Option<String>,
    schema_path: PathBuf,
}

impl CodexCliAdapter {
    pub fn with_config(
        model: Option<String>,
        effort: Option<String>,
        environment: SeatEnvironment,
        usage_sink: Option<UsageSink>,
    ) -> CouncilResult<Self> {
        let binary = cli_binary(AgentId::Gpt);
        verify_codex_subscription(&binary)?;
        Ok(Self {
            usage_sink,
            environment,
            binary,
            model: model.or_else(|| std::env::var("CODEX_SUBSCRIPTION_MODEL").ok()),
            effort: effort.or_else(|| std::env::var("CODEX_SUBSCRIPTION_EFFORT").ok()),
            schema_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../src/intent.schema.json"),
        })
    }

    async fn invoke(&self, prompt: String, structured: bool) -> CouncilResult<String> {
        let mut command = Command::new(&self.binary);
        command.arg("exec").arg("--ephemeral");
        let tools = if structured {
            SeatTools::NONE
        } else {
            self.tools()
        };
        match (&self.environment.artifacts, tools.write) {
            // A tool speak turn: writes are mechanically confined to the
            // artifacts folder (with the /tmp escape hatches closed) and web
            // search is on. Codex cannot confine reads to a directory set —
            // the read boundary is instruction-level only (council rule 10).
            (Some(artifacts), true) => {
                command
                    .arg("--sandbox")
                    .arg("workspace-write")
                    .arg("--cd")
                    .arg(artifacts)
                    .arg("--config")
                    .arg("web_search=\"live\"")
                    .arg("--config")
                    .arg("sandbox_workspace_write.exclude_slash_tmp=true")
                    .arg("--config")
                    .arg("sandbox_workspace_write.exclude_tmpdir_env_var=true");
            }
            _ => {
                command.arg("--sandbox").arg("read-only");
            }
        }
        command
            .arg("--skip-git-repo-check")
            .arg("--ignore-user-config")
            .arg("--json");
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        if let Some(effort) = &self.effort {
            command
                .arg("--config")
                .arg(format!("model_reasoning_effort=\"{effort}\""));
        }
        if structured {
            command.arg("--output-schema").arg(&self.schema_path);
        }
        command.arg("-");
        remove_metered_api_environment(&mut command);
        let output = run_child_within(
            command,
            prompt,
            AgentId::Gpt,
            turn_limit(&self.environment, structured),
        )
        .await?;
        for line in output.lines() {
            if let Ok(event) = serde_json::from_str::<Value>(line)
                && event.get("type").and_then(Value::as_str) == Some("turn.completed")
            {
                report_usage(&self.usage_sink, AgentId::Gpt, &event);
            }
        }
        parse_codex_message(&output)
    }
}

#[async_trait]
impl AgentAdapter for CodexCliAdapter {
    fn model_label(&self) -> Option<String> {
        self.model.clone()
    }

    fn id(&self) -> AgentId {
        AgentId::Gpt
    }

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        let prompt = format!(
            "{}\n\n{}",
            evaluation_instructions(self.id()),
            evaluation_input(room)
        );
        let message = self.invoke(prompt, true).await?;
        parse_decision(&message, event.id)
    }

    async fn speak(&self, room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        let prompt = format!(
            "{}\n\n{}",
            speak_instructions(self.id(), &self.environment, self.tools()),
            speaking_input(room)
        );
        non_empty_speech(self.id(), self.invoke(prompt, false).await?)
    }
}

pub struct ClaudeCliAdapter {
    usage_sink: Option<UsageSink>,
    environment: SeatEnvironment,
    binary: String,
    model: String,
}

impl ClaudeCliAdapter {
    /// The Claude CLI has no reasoning-effort flag; `_effort` is accepted for
    /// signature uniformity and ignored.
    pub fn with_config(
        model: Option<String>,
        _effort: Option<String>,
        environment: SeatEnvironment,
        usage_sink: Option<UsageSink>,
    ) -> CouncilResult<Self> {
        let binary = cli_binary(AgentId::Claude);
        verify_claude_subscription(&binary)?;
        Ok(Self {
            usage_sink,
            environment,
            binary,
            model: model
                .or_else(|| std::env::var("CLAUDE_SUBSCRIPTION_MODEL").ok())
                .unwrap_or_else(|| "sonnet".to_owned()),
        })
    }

    async fn invoke(
        &self,
        system_prompt: String,
        prompt: String,
        structured: bool,
    ) -> CouncilResult<String> {
        let tools = if structured {
            SeatTools::NONE
        } else {
            self.tools()
        };
        let mut command = Command::new(&self.binary);
        command.args(claude_args(
            &self.model,
            &system_prompt,
            structured,
            &self.environment,
            tools,
        ));
        // --restricted confines file tools to cwd plus every --add-dir, so the
        // working directory is part of the boundary.
        if let (Some(artifacts), true) = (&self.environment.artifacts, tools.read) {
            command.current_dir(
                self.environment
                    .workspace
                    .as_deref()
                    .unwrap_or(artifacts.as_path()),
            );
        }
        remove_metered_api_environment(&mut command);
        let output = run_child_within(
            command,
            prompt,
            AgentId::Claude,
            turn_limit(&self.environment, structured),
        )
        .await?;
        if let Ok(value) = serde_json::from_str::<Value>(&output) {
            report_usage(&self.usage_sink, AgentId::Claude, &value);
        }
        parse_claude_message(&output, structured)
    }
}

/// The child `claude` invocation for one council turn.
///
/// No `--permission-mode`: `--tools ""` already leaves the child with no tools,
/// so a permission mode would only govern tools that do not exist — and plan
/// mode carried its own framing into the room. Three sessions in the quality
/// batch have Claude opening its turn by explaining that the room is not a
/// codebase and that "plan mode 워크플로우(Explore → Plan → 파일 작성)" does not
/// apply, which is Claude Code's plan-mode language reaching the transcript.
fn claude_args(
    model: &str,
    system_prompt: &str,
    structured: bool,
    environment: &SeatEnvironment,
    tools: SeatTools,
) -> Vec<String> {
    let mut args = vec!["--print".to_owned(), "--safe-mode".to_owned()];
    match (&environment.artifacts, tools.any()) {
        // A tool speak turn. --restricted draws a mechanical boundary around
        // cwd (the workspace, or the artifacts dir when no workspace is set)
        // plus the added artifacts dir; dontAsk denies anything else instead
        // of hanging on an approval prompt. That boundary is one combined
        // read+write set, so `seat_tools` grants Write only when the boundary
        // is the artifacts folder alone — with a workspace present, the Write
        // tool is simply not in the list, and the workspace stays read-only by
        // construction. --permission-mode plan is never used — its framing
        // reached the transcript as speech (see the v1 sessions).
        (Some(artifacts), true) => {
            let mut names: Vec<&str> = Vec::new();
            if tools.web {
                names.push("WebSearch");
            }
            if tools.read {
                names.extend(["Read", "Grep", "Glob"]);
            }
            if tools.write {
                names.push("Write");
            }
            args.push("--restricted".to_owned());
            if environment.workspace.is_some() {
                args.push("--add-dir".to_owned());
                args.push(artifacts.to_string_lossy().into_owned());
            }
            args.push("--tools".to_owned());
            args.push(names.join(","));
            args.push("--allowedTools".to_owned());
            args.push(names.join(" "));
            args.push("--permission-mode".to_owned());
            args.push("dontAsk".to_owned());
        }
        // Judgements and tool-less rooms: no tools at all, so there is
        // nothing for a permission mode to govern.
        _ => {
            args.push("--tools".to_owned());
            args.push(String::new());
        }
    }
    args.extend([
        "--no-session-persistence".to_owned(),
        "--output-format".to_owned(),
        "json".to_owned(),
        "--model".to_owned(),
        model.to_owned(),
        "--system-prompt".to_owned(),
        system_prompt.to_owned(),
    ]);
    if structured {
        args.push("--json-schema".to_owned());
        args.push(INTENT_SCHEMA.to_owned());
    }
    args
}

#[async_trait]
impl AgentAdapter for ClaudeCliAdapter {
    fn model_label(&self) -> Option<String> {
        Some(self.model.clone())
    }

    fn id(&self) -> AgentId {
        AgentId::Claude
    }

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        let message = self
            .invoke(
                evaluation_instructions(self.id()),
                evaluation_input(room),
                true,
            )
            .await?;
        parse_decision(&message, event.id)
    }

    async fn speak(&self, room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        let message = self
            .invoke(
                speak_instructions(self.id(), &self.environment, self.tools()),
                speaking_input(room),
                false,
            )
            .await?;
        non_empty_speech(self.id(), message)
    }
}

pub struct GrokCliAdapter {
    usage_sink: Option<UsageSink>,
    environment: SeatEnvironment,
    binary: String,
    model: Option<String>,
    effort: Option<String>,
}

impl GrokCliAdapter {
    pub fn with_config(
        model: Option<String>,
        effort: Option<String>,
        environment: SeatEnvironment,
        usage_sink: Option<UsageSink>,
    ) -> CouncilResult<Self> {
        let binary = cli_binary(AgentId::Grok);
        verify_grok_subscription(&binary)?;
        if let Some(artifacts) = &environment.artifacts {
            write_grok_sandbox_profile(artifacts, environment.workspace.as_deref())
                .map_err(|error| CouncilError::provider(AgentId::Grok, error))?;
        }
        Ok(Self {
            usage_sink,
            environment,
            binary,
            model: model.or_else(|| std::env::var("GROK_SUBSCRIPTION_MODEL").ok()),
            effort: effort.or_else(|| std::env::var("GROK_SUBSCRIPTION_EFFORT").ok()),
        })
    }

    async fn invoke(
        &self,
        rules: String,
        prompt: String,
        structured: bool,
    ) -> CouncilResult<String> {
        // Grok's agentic runtime may still try read-only tools in plan mode
        // (council rule 9 bans them), so a small turn budget is left for the
        // final message.
        let mut command = Command::new(&self.binary);
        command
            .arg("--single")
            .arg(prompt)
            .arg("--rules")
            .arg(rules)
            .arg("--output-format")
            .arg("json")
            .arg("--no-subagents");
        let tools = if structured {
            SeatTools::NONE
        } else {
            self.tools()
        };
        match (&self.environment.artifacts, tools.write) {
            // A tool speak turn: the kernel sandbox (written by with_config as
            // <artifacts>/.grok/sandbox.toml) makes the workspace read-only
            // and the artifacts dir the only writable path — the strongest
            // confinement of the four seats. bypassPermissions is safe here
            // because the sandbox, not the permission layer, is the boundary.
            (Some(artifacts), true) => {
                command
                    .arg("--permission-mode")
                    .arg("bypassPermissions")
                    .arg("--sandbox")
                    .arg("council")
                    .arg("--max-turns")
                    .arg("6")
                    .current_dir(artifacts);
            }
            _ => {
                command
                    .arg("--permission-mode")
                    .arg("plan")
                    .arg("--disable-web-search")
                    .arg("--max-turns")
                    .arg("4");
            }
        }
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        if let Some(effort) = &self.effort {
            command.arg("--reasoning-effort").arg(effort);
        }
        if structured {
            command.arg("--json-schema").arg(INTENT_SCHEMA);
        }
        remove_metered_api_environment(&mut command);
        let output = run_child_within(
            command,
            String::new(),
            AgentId::Grok,
            turn_limit(&self.environment, structured),
        )
        .await?;
        if let Ok(value) = serde_json::from_str::<Value>(&output) {
            report_usage(&self.usage_sink, AgentId::Grok, &value);
        }
        parse_grok_message(&output)
    }
}

#[async_trait]
impl AgentAdapter for GrokCliAdapter {
    fn model_label(&self) -> Option<String> {
        self.model.clone()
    }

    fn id(&self) -> AgentId {
        AgentId::Grok
    }

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        let message = self
            .invoke(
                evaluation_instructions(self.id()),
                evaluation_input(room),
                true,
            )
            .await?;
        parse_decision(&message, event.id)
    }

    async fn speak(&self, room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        let message = self
            .invoke(
                speak_instructions(self.id(), &self.environment, self.tools()),
                speaking_input(room),
                false,
            )
            .await?;
        non_empty_speech(self.id(), message)
    }
}

pub struct AntigravityCliAdapter {
    usage_sink: Option<UsageSink>,
    environment: SeatEnvironment,
    binary: String,
    model: String,
    effort: Option<String>,
}

impl AntigravityCliAdapter {
    pub fn with_config(
        model: Option<String>,
        effort: Option<String>,
        environment: SeatEnvironment,
        usage_sink: Option<UsageSink>,
    ) -> CouncilResult<Self> {
        let binary = cli_binary(AgentId::Gemini);
        verify_antigravity_subscription(&binary)?;
        Ok(Self {
            usage_sink,
            environment,
            binary,
            model: model
                .or_else(|| std::env::var("GEMINI_SUBSCRIPTION_MODEL").ok())
                .unwrap_or_else(|| "gemini-3.7-flash-high".to_owned()),
            effort: effort.or_else(|| std::env::var("GEMINI_SUBSCRIPTION_EFFORT").ok()),
        })
    }

    async fn invoke(&self, prompt: String, structured: bool) -> CouncilResult<String> {
        let mut command = Command::new(&self.binary);
        command
            .arg("--print")
            .arg(prompt)
            .arg("--output-format")
            .arg("json")
            .arg("--sandbox")
            .arg("--model")
            .arg(&self.model);
        if let Some(effort) = &self.effort {
            command.arg("--effort").arg(effort);
        }
        if structured {
            command.arg("--json-schema").arg(INTENT_SCHEMA);
        }
        remove_metered_api_environment(&mut command);
        let output = run_child_within(
            command,
            String::new(),
            AgentId::Gemini,
            turn_limit(&self.environment, structured),
        )
        .await?;
        if let Ok(value) = serde_json::from_str::<Value>(&output) {
            report_usage(&self.usage_sink, AgentId::Gemini, &value);
        }
        parse_antigravity_message(&output, structured)
    }
}

#[async_trait]
impl AgentAdapter for AntigravityCliAdapter {
    fn model_label(&self) -> Option<String> {
        Some(self.model.clone())
    }

    fn id(&self) -> AgentId {
        AgentId::Gemini
    }

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        // The Antigravity CLI has no system-prompt flag, so instructions and
        // input travel in one prompt.
        let prompt = format!(
            "{}\n\n{}",
            evaluation_instructions(self.id()),
            evaluation_input(room)
        );
        let message = self.invoke(prompt, true).await?;
        parse_decision(&message, event.id)
    }

    async fn speak(&self, room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        let prompt = format!(
            "{}\n\n{}",
            speak_instructions(self.id(), &self.environment, self.tools()),
            speaking_input(room)
        );
        non_empty_speech(self.id(), self.invoke(prompt, false).await?)
    }
}

/// The per-session Grok sandbox: kernel-enforced, read-only workspace,
/// writable artifacts. Grok reads `<cwd>/.grok/sandbox.toml`, and the tool
/// speak turn runs with cwd set to the artifacts dir. The base `strict`
/// profile keeps /tmp writable no matter what, which is why the artifacts and
/// workspace folders live under the repo's outputs/, never under /tmp.
fn write_grok_sandbox_profile(
    artifacts: &std::path::Path,
    workspace: Option<&std::path::Path>,
) -> std::io::Result<()> {
    let dir = artifacts.join(".grok");
    std::fs::create_dir_all(&dir)?;
    let read_only = workspace
        .map(|path| format!("read_only = [{:?}]\n", path.to_string_lossy()))
        .unwrap_or_default();
    let profile = format!(
        "[profiles.council]\nextends = \"strict\"\n{read_only}read_write = [{:?}]\n",
        artifacts.to_string_lossy()
    );
    std::fs::write(dir.join("sandbox.toml"), profile)
}

/// Each seat's grant comes from the single source in `providers::seat_tools`;
/// the prompt and the CLI arguments both read it through this method.
macro_rules! seat_tools_method {
    ($adapter:ty, $agent:expr) => {
        impl $adapter {
            fn tools(&self) -> SeatTools {
                seat_tools(ProviderKind::Subscription, $agent, &self.environment)
            }
        }
    };
}
seat_tools_method!(CodexCliAdapter, AgentId::Gpt);
seat_tools_method!(ClaudeCliAdapter, AgentId::Claude);
seat_tools_method!(GrokCliAdapter, AgentId::Grok);
seat_tools_method!(AntigravityCliAdapter, AgentId::Gemini);

/// Judgements stay on the short limit; a speaking turn in a tool room may
/// search or do requested work and gets the longer one.
fn turn_limit(environment: &SeatEnvironment, structured: bool) -> Duration {
    if !structured && environment.artifacts.is_some() {
        TOOL_TURN_TIMEOUT
    } else {
        PROCESS_TIMEOUT
    }
}

async fn run_child_within(
    mut command: Command,
    prompt: String,
    agent: AgentId,
    limit: Duration,
) -> CouncilResult<String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| CouncilError::provider(agent, error))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CouncilError::provider(agent, "failed to open CLI stdin"))?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .map_err(|error| CouncilError::provider(agent, error))?;
    stdin
        .shutdown()
        .await
        .map_err(|error| CouncilError::provider(agent, error))?;
    drop(stdin);

    let output = timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| {
            CouncilError::provider(
                agent,
                format!("CLI timed out after {} seconds", limit.as_secs()),
            )
        })?
        .map_err(|error| CouncilError::provider(agent, error))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CouncilError::provider(
            agent,
            format!(
                "CLI exited with {}: stdout={} stderr={}",
                output.status,
                stdout.trim(),
                stderr.trim()
            ),
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| CouncilError::provider(agent, format!("non-UTF-8 CLI output: {error}")))
}

/// Checks the CLI login for one agent without calling any model.
/// Login probes shell out to a CLI that may be momentarily unable to answer
/// (keychain, a config lock held by another instance, a slow first start):
/// Grok's probe failed at three server starts and passed on the retry each
/// time. A genuine logout still fails — after the last attempt.
const AUTH_PROBE_ATTEMPTS: u32 = 3;
const AUTH_PROBE_BACKOFF: Duration = Duration::from_millis(400);

fn retrying(mut probe: impl FnMut() -> CouncilResult<()>) -> CouncilResult<()> {
    let mut last = None;
    for attempt in 1..=AUTH_PROBE_ATTEMPTS {
        match probe() {
            Ok(()) => return Ok(()),
            Err(error) => last = Some(error),
        }
        if attempt < AUTH_PROBE_ATTEMPTS {
            std::thread::sleep(AUTH_PROBE_BACKOFF * attempt);
        }
    }
    Err(last.expect("at least one attempt ran"))
}

pub fn check_subscription(agent: AgentId) -> CouncilResult<()> {
    let binary = cli_binary(agent);
    match agent {
        AgentId::Gpt => verify_codex_subscription(&binary),
        AgentId::Claude => verify_claude_subscription(&binary),
        AgentId::Gemini => verify_antigravity_subscription(&binary),
        AgentId::Grok => verify_grok_subscription(&binary),
    }
}

fn verify_codex_subscription(binary: &str) -> CouncilResult<()> {
    retrying(|| probe_codex_subscription(binary))
}

fn probe_codex_subscription(binary: &str) -> CouncilResult<()> {
    let mut command = SyncCommand::new(binary);
    command.args(["login", "status"]);
    remove_metered_api_environment_sync(&mut command);
    let output = command
        .output()
        .map_err(|error| CouncilError::provider(AgentId::Gpt, error))?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() || !combined.contains("Logged in using ChatGPT") {
        return Err(CouncilError::provider(
            AgentId::Gpt,
            "Codex CLI is not logged in using a ChatGPT subscription",
        ));
    }
    Ok(())
}

fn verify_claude_subscription(binary: &str) -> CouncilResult<()> {
    retrying(|| probe_claude_subscription(binary))
}

fn probe_claude_subscription(binary: &str) -> CouncilResult<()> {
    let mut command = SyncCommand::new(binary);
    command.args(["auth", "status", "--json"]);
    remove_metered_api_environment_sync(&mut command);
    let output = command
        .output()
        .map_err(|error| CouncilError::provider(AgentId::Claude, error))?;
    let status: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| CouncilError::provider(AgentId::Claude, error))?;
    let subscription_auth = status.get("loggedIn").and_then(Value::as_bool) == Some(true)
        && status.get("authMethod").and_then(Value::as_str) == Some("claude.ai")
        && status
            .get("subscriptionType")
            .and_then(Value::as_str)
            .is_some();
    if !output.status.success() || !subscription_auth {
        return Err(CouncilError::provider(
            AgentId::Claude,
            "Claude CLI is not logged in using a Claude subscription",
        ));
    }
    Ok(())
}

fn verify_grok_subscription(binary: &str) -> CouncilResult<()> {
    retrying(|| probe_grok_subscription(binary))
}

fn probe_grok_subscription(binary: &str) -> CouncilResult<()> {
    let mut command = SyncCommand::new(binary);
    command.arg("models");
    remove_metered_api_environment_sync(&mut command);
    let output = command
        .output()
        .map_err(|error| CouncilError::provider(AgentId::Grok, error))?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() || !combined.contains("logged in with grok.com") {
        return Err(CouncilError::provider(
            AgentId::Grok,
            "Grok CLI is not logged in with a grok.com subscription (run `grok login`)",
        ));
    }
    Ok(())
}

fn verify_antigravity_subscription(binary: &str) -> CouncilResult<()> {
    retrying(|| probe_antigravity_subscription(binary))
}

fn probe_antigravity_subscription(binary: &str) -> CouncilResult<()> {
    let mut command = SyncCommand::new(binary);
    command.arg("models");
    remove_metered_api_environment_sync(&mut command);
    let output = command
        .output()
        .map_err(|error| CouncilError::provider(AgentId::Gemini, error))?;
    let listed_models = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim_start().starts_with("gemini"));
    if !output.status.success() || !listed_models {
        return Err(CouncilError::provider(
            AgentId::Gemini,
            "Antigravity CLI is not logged in (open Antigravity and sign in with a Google account)",
        ));
    }
    Ok(())
}

fn remove_metered_api_environment(command: &mut Command) {
    for variable in METERED_API_VARIABLES {
        command.env_remove(variable);
    }
}

fn remove_metered_api_environment_sync(command: &mut SyncCommand) {
    for variable in METERED_API_VARIABLES {
        command.env_remove(variable);
    }
}

const METERED_API_VARIABLES: [&str; 11] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "AWS_BEARER_TOKEN_BEDROCK",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "XAI_API_KEY",
    "GROK_API_KEY",
];

fn parse_codex_message(output: &str) -> CouncilResult<String> {
    let mut last_message = None;
    for line in output.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("item.completed") {
            continue;
        }
        let Some(item) = event.get("item") else {
            continue;
        };
        if item.get("type").and_then(Value::as_str) == Some("agent_message")
            && let Some(text) = item.get("text").and_then(Value::as_str)
        {
            last_message = Some(text.to_owned());
        }
    }
    last_message
        .ok_or_else(|| CouncilError::provider(AgentId::Gpt, "no final agent_message in CLI JSONL"))
}

fn parse_claude_message(output: &str, structured: bool) -> CouncilResult<String> {
    let value: Value = serde_json::from_str(output)
        .map_err(|error| CouncilError::provider(AgentId::Claude, error))?;
    if value.get("is_error").and_then(Value::as_bool) == Some(true) {
        let message = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("Claude CLI returned an error result");
        return Err(CouncilError::provider(AgentId::Claude, message));
    }
    if structured && let Some(structured_output) = value.get("structured_output") {
        return serde_json::to_string(structured_output)
            .map_err(|error| CouncilError::provider(AgentId::Claude, error));
    }
    value
        .get("result")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CouncilError::provider(AgentId::Claude, "no result in CLI JSON output"))
}

fn parse_grok_message(output: &str) -> CouncilResult<String> {
    let value: Value = serde_json::from_str(output)
        .map_err(|error| CouncilError::provider(AgentId::Grok, error))?;
    value
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CouncilError::provider(AgentId::Grok, "no text in CLI JSON output"))
}

fn parse_antigravity_message(output: &str, structured: bool) -> CouncilResult<String> {
    let value: Value = serde_json::from_str(output)
        .map_err(|error| CouncilError::provider(AgentId::Gemini, error))?;
    if value.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown status");
        return Err(CouncilError::provider(
            AgentId::Gemini,
            format!("Antigravity CLI returned {status}"),
        ));
    }
    if structured && let Some(structured_output) = value.get("structured_output") {
        return serde_json::to_string(structured_output)
            .map_err(|error| CouncilError::provider(AgentId::Gemini, error));
    }
    value
        .get("response")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CouncilError::provider(AgentId::Gemini, "no response in CLI JSON output"))
}

fn non_empty_speech(agent: AgentId, message: String) -> CouncilResult<String> {
    let speech = message.trim();
    if speech.is_empty() {
        return Err(CouncilError::provider(agent, "empty speech response"));
    }
    Ok(speech.to_owned())
}

#[cfg(test)]
mod tests {
    use super::claude_args;
    use super::{
        ProviderKind, SeatEnvironment, SeatTools, parse_antigravity_message, parse_claude_message,
        parse_codex_message, parse_grok_message, seat_tools, speak_instructions,
    };
    use crate::adapter::CouncilError;
    use crate::model::AgentId;

    #[test]
    fn a_probe_that_recovers_within_the_budget_passes() {
        let mut calls = 0;
        let result = super::retrying(|| {
            calls += 1;
            if calls < 3 {
                Err(CouncilError::provider(
                    AgentId::Grok,
                    "not logged in (transient)",
                ))
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok());
        assert_eq!(calls, 3);
    }

    #[test]
    fn a_probe_that_keeps_failing_reports_the_last_error_after_the_budget() {
        let mut calls = 0;
        let result = super::retrying(|| {
            calls += 1;
            Err(CouncilError::provider(
                AgentId::Grok,
                format!("attempt {calls}"),
            ))
        });
        assert_eq!(calls, super::AUTH_PROBE_ATTEMPTS);
        assert!(result.unwrap_err().to_string().contains("attempt 3"));
    }

    #[test]
    fn a_probe_that_passes_first_time_is_not_repeated() {
        let mut calls = 0;
        assert!(
            super::retrying(|| {
                calls += 1;
                Ok(())
            })
            .is_ok()
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn parses_codex_jsonl_final_message() {
        let output = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"x\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"decision\\\":\\\"PASS\\\",\\\"reason\\\":\\\"\\\"}\"}}\n"
        );
        assert_eq!(
            parse_codex_message(output).unwrap(),
            r#"{"decision":"PASS","reason":""}"#
        );
    }

    #[test]
    fn parses_claude_structured_output() {
        let output = r#"{"result":"ignored","structured_output":{"decision":"REQUEST_FLOOR","reason":"new"}}"#;
        assert_eq!(
            parse_claude_message(output, true).unwrap(),
            r#"{"decision":"REQUEST_FLOOR","reason":"new"}"#
        );
    }

    #[test]
    fn rejects_claude_error_result_as_public_speech() {
        let output = r#"{"is_error":true,"result":"weekly limit reached","total_cost_usd":0}"#;
        let error = parse_claude_message(output, false).unwrap_err();
        assert!(error.to_string().contains("weekly limit reached"));
    }

    #[test]
    fn parses_grok_text_field() {
        let output =
            r#"{"text":"{\"decision\":\"PASS\",\"reason\":\"\"}","stopReason":"end_turn"}"#;
        assert_eq!(
            parse_grok_message(output).unwrap(),
            r#"{"decision":"PASS","reason":""}"#
        );
    }

    #[test]
    fn parses_antigravity_structured_output_and_rejects_failure() {
        let ok = r#"{"status":"SUCCESS","response":"ignored","structured_output":{"decision":"REQUEST_FLOOR","reason":"new"}}"#;
        assert_eq!(
            parse_antigravity_message(ok, true).unwrap(),
            r#"{"decision":"REQUEST_FLOOR","reason":"new"}"#
        );
        let failed = r#"{"status":"FAILED","response":"quota"}"#;
        assert!(parse_antigravity_message(failed, false).is_err());
    }

    #[test]
    fn a_toolless_claude_child_gets_no_tools_and_no_permission_mode() {
        let args = claude_args(
            "sonnet",
            "규칙",
            false,
            &SeatEnvironment::default(),
            SeatTools::NONE,
        );

        // Every tool off, so there is nothing for a permission mode to govern.
        let tools = args
            .iter()
            .position(|arg| arg == "--tools")
            .expect("--tools");
        assert_eq!(args[tools + 1], "");
        assert!(args.iter().any(|arg| arg == "--safe-mode"));
        // Plan mode put its own framing in Claude's mouth; see claude_args.
        assert!(!args.iter().any(|arg| arg == "--permission-mode"));
    }

    fn tool_room() -> SeatEnvironment {
        SeatEnvironment {
            language: None,
            workspace: Some(std::path::PathBuf::from("/Users/x/proj")),
            artifacts: Some(std::path::PathBuf::from(
                "/Users/x/repo/outputs/artifacts/s1",
            )),
        }
    }

    fn claude_grant(env: &SeatEnvironment) -> SeatTools {
        seat_tools(ProviderKind::Subscription, AgentId::Claude, env)
    }

    #[test]
    fn a_judgement_in_a_tool_room_still_runs_without_tools() {
        // Judgement turns always pass NONE regardless of the room's grant.
        let args = claude_args("sonnet", "규칙", true, &tool_room(), SeatTools::NONE);

        let tools = args
            .iter()
            .position(|arg| arg == "--tools")
            .expect("--tools");
        assert_eq!(args[tools + 1], "");
        assert!(!args.iter().any(|arg| arg == "--restricted"));
        assert!(args.iter().any(|arg| arg == "--json-schema"));
    }

    #[test]
    fn a_tool_speak_turn_is_restricted_and_never_uses_plan_mode() {
        let env = tool_room();
        let args = claude_args("sonnet", "규칙", false, &env, claude_grant(&env));

        assert!(args.iter().any(|arg| arg == "--restricted"));
        // The artifacts dir joins the boundary; the workspace is the cwd.
        let add = args
            .iter()
            .position(|arg| arg == "--add-dir")
            .expect("--add-dir");
        assert_eq!(args[add + 1], "/Users/x/repo/outputs/artifacts/s1");
        let mode = args
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("--permission-mode");
        assert_eq!(args[mode + 1], "dontAsk");
        let tools = args
            .iter()
            .position(|arg| arg == "--tools")
            .expect("--tools");
        // With a workspace in the boundary, Write is withheld: --restricted
        // is one combined read+write set, and the workspace must stay
        // read-only by construction, not by instruction.
        assert_eq!(args[tools + 1], "WebSearch,Read,Grep,Glob");
        assert!(!args.iter().any(|arg| arg == "plan"));
        assert!(!args.iter().any(|arg| arg == "--json-schema"));
    }

    #[test]
    fn without_a_workspace_the_claude_boundary_is_artifacts_only_so_write_is_granted() {
        let env = SeatEnvironment {
            language: None,
            workspace: None,
            artifacts: Some(std::path::PathBuf::from(
                "/Users/x/repo/outputs/artifacts/s1",
            )),
        };
        let grant = claude_grant(&env);
        assert!(grant.write);
        assert!(!grant.run);

        let args = claude_args("sonnet", "규칙", false, &env, grant);
        let tools = args
            .iter()
            .position(|arg| arg == "--tools")
            .expect("--tools");
        assert_eq!(args[tools + 1], "WebSearch,Read,Grep,Glob,Write");
        // No --add-dir: cwd is the artifacts dir and that is the whole boundary.
        assert!(!args.iter().any(|arg| arg == "--add-dir"));
    }

    #[test]
    fn the_prompt_and_the_arguments_read_the_same_grant() {
        // The invariant the design leans on: whatever `seat_tools` says is
        // exactly what the child gets and exactly what the model is told.
        for env in [tool_room(), SeatEnvironment::default()] {
            let grant = claude_grant(&env);
            let args = claude_args("sonnet", "규칙", false, &env, grant);
            let listed = args
                .iter()
                .position(|arg| arg == "--tools")
                .map(|at| args[at + 1].clone())
                .expect("--tools");
            assert_eq!(listed.contains("Write"), grant.write);
            assert_eq!(listed.contains("WebSearch"), grant.web);
            assert_eq!(listed.contains("Read"), grant.read);

            let prompt = speak_instructions(AgentId::Claude, &env, grant);
            assert_eq!(
                prompt.contains("creating files inside the artifacts folder"),
                env.artifacts.is_some()
            );
            if grant.any() && !grant.write {
                assert!(prompt.contains("Not granted this turn: creating files"));
            }
        }
    }

    #[test]
    fn gemini_is_granted_the_web_and_nothing_else() {
        let grant = seat_tools(ProviderKind::Subscription, AgentId::Gemini, &tool_room());
        assert_eq!(
            grant,
            SeatTools {
                web: true,
                ..SeatTools::NONE
            }
        );
        let prompt = speak_instructions(AgentId::Gemini, &tool_room(), grant);
        assert!(prompt.contains("these tools and no others: web search."));
    }

    #[test]
    fn a_toolless_room_grants_nothing_to_anyone() {
        for agent in AgentId::ORDER {
            assert_eq!(
                seat_tools(
                    ProviderKind::Subscription,
                    agent,
                    &SeatEnvironment::default()
                ),
                SeatTools::NONE
            );
            assert_eq!(
                seat_tools(ProviderKind::Mock, agent, &tool_room()),
                SeatTools::NONE
            );
        }
    }

    #[test]
    fn only_a_judgement_call_pins_the_intent_schema() {
        let env = SeatEnvironment::default();
        assert!(
            !claude_args("sonnet", "규칙", false, &env, SeatTools::NONE)
                .iter()
                .any(|arg| arg == "--json-schema")
        );
        assert!(
            claude_args("sonnet", "규칙", true, &env, SeatTools::NONE)
                .iter()
                .any(|arg| arg == "--json-schema")
        );
    }
}
