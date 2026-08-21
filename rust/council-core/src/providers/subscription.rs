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
use crate::prompts::{
    evaluation_input, evaluation_instructions, speaking_input, speaking_instructions,
};

use super::parse_decision;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(180);
const INTENT_SCHEMA: &str = include_str!("../../../../src/intent.schema.json");

pub struct CodexCliAdapter {
    binary: String,
    model: Option<String>,
    schema_path: PathBuf,
}

impl CodexCliAdapter {
    pub fn subscription() -> CouncilResult<Self> {
        let binary = std::env::var("CODEX_CLI_BIN").unwrap_or_else(|_| "codex".to_owned());
        verify_codex_subscription(&binary)?;
        Ok(Self {
            binary,
            model: std::env::var("CODEX_SUBSCRIPTION_MODEL").ok(),
            schema_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../src/intent.schema.json"),
        })
    }

    async fn invoke(&self, prompt: String, structured: bool) -> CouncilResult<String> {
        let mut command = Command::new(&self.binary);
        command
            .arg("exec")
            .arg("--ephemeral")
            .arg("--sandbox")
            .arg("read-only")
            .arg("--skip-git-repo-check")
            .arg("--ignore-user-config")
            .arg("--json");
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        if structured {
            command.arg("--output-schema").arg(&self.schema_path);
        }
        command.arg("-");
        remove_metered_api_environment(&mut command);
        let output = run_child(command, prompt, AgentId::Gpt).await?;
        parse_codex_message(&output)
    }
}

#[async_trait]
impl AgentAdapter for CodexCliAdapter {
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
            speaking_instructions(self.id()),
            speaking_input(room)
        );
        non_empty_speech(self.id(), self.invoke(prompt, false).await?)
    }
}

pub struct ClaudeCliAdapter {
    binary: String,
    model: String,
}

impl ClaudeCliAdapter {
    pub fn subscription() -> CouncilResult<Self> {
        let binary = std::env::var("CLAUDE_CLI_BIN").unwrap_or_else(|_| "claude".to_owned());
        verify_claude_subscription(&binary)?;
        Ok(Self {
            binary,
            model: std::env::var("CLAUDE_SUBSCRIPTION_MODEL")
                .unwrap_or_else(|_| "sonnet".to_owned()),
        })
    }

    async fn invoke(
        &self,
        system_prompt: String,
        prompt: String,
        structured: bool,
    ) -> CouncilResult<String> {
        let mut command = Command::new(&self.binary);
        command
            .arg("--print")
            .arg("--safe-mode")
            .arg("--tools")
            .arg("")
            .arg("--permission-mode")
            .arg("plan")
            .arg("--no-session-persistence")
            .arg("--output-format")
            .arg("json")
            .arg("--model")
            .arg(&self.model)
            .arg("--system-prompt")
            .arg(system_prompt);
        if structured {
            command.arg("--json-schema").arg(INTENT_SCHEMA);
        }
        remove_metered_api_environment(&mut command);
        let output = run_child(command, prompt, AgentId::Claude).await?;
        parse_claude_message(&output, structured)
    }
}

#[async_trait]
impl AgentAdapter for ClaudeCliAdapter {
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
                speaking_instructions(self.id()),
                speaking_input(room),
                false,
            )
            .await?;
        non_empty_speech(self.id(), message)
    }
}

async fn run_child(mut command: Command, prompt: String, agent: AgentId) -> CouncilResult<String> {
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

    let output = timeout(PROCESS_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| CouncilError::provider(agent, "CLI timed out after 180 seconds"))?
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

fn verify_codex_subscription(binary: &str) -> CouncilResult<()> {
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

fn remove_metered_api_environment(command: &mut Command) {
    for variable in metered_api_variables() {
        command.env_remove(variable);
    }
}

fn remove_metered_api_environment_sync(command: &mut SyncCommand) {
    for variable in metered_api_variables() {
        command.env_remove(variable);
    }
}

fn metered_api_variables() -> [&'static str; 7] {
    [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "AWS_BEARER_TOKEN_BEDROCK",
    ]
}

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

fn non_empty_speech(agent: AgentId, message: String) -> CouncilResult<String> {
    let speech = message.trim();
    if speech.is_empty() {
        return Err(CouncilError::provider(agent, "empty speech response"));
    }
    Ok(speech.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{parse_claude_message, parse_codex_message};

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
}
