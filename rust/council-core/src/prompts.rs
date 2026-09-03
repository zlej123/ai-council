use crate::model::{AgentId, RoomSnapshot};

pub const COUNCIL_RULES: &str = include_str!("../../../src/council_rules.txt");

pub fn transcript(room: &RoomSnapshot) -> String {
    room.events
        .iter()
        .map(|event| format!("Event #{}\n{}: {}", event.id, event.author, event.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn evaluation_instructions(agent: AgentId) -> String {
    format!(
        "{COUNCIL_RULES}\nYou are {agent}. Evaluate the newest event after reading the complete transcript. \
Return only a JSON object. decision must be PASS or REQUEST_FLOOR. reason must be \
a short internal reason and must not contain a proposed speech. Do not answer the room yet. \
This judgement turn grants no tools: do not search, read, write, or run anything; decide from \
the transcript alone."
    )
}

pub fn evaluation_input(room: &RoomSnapshot) -> String {
    format!(
        "Complete authoritative Council Room transcript:\n\n{}\n\nDecide whether to request the floor for the newest event.",
        transcript(room)
    )
}

pub fn speaking_instructions(agent: AgentId) -> String {
    speaking_instructions_in(agent, None)
}

/// The room's speech languages, keyed by the codes the settings panel offers.
/// An unknown code adds no directive — the model then follows the room, which
/// is also what omitting the language means.
pub fn language_name(code: &str) -> Option<&'static str> {
    match code {
        "ko" => Some("Korean"),
        "en" => Some("English"),
        "ja" => Some("Japanese"),
        "zh" => Some("Chinese"),
        _ => None,
    }
}

/// What one speaking turn may actually do. Derived per seat by
/// `providers::seat_tools` and consumed by BOTH the prompt and the CLI
/// arguments, so the model is never told about a capability the child
/// process does not have, nor denied one it has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeatTools {
    pub web: bool,
    pub read: bool,
    pub write: bool,
    pub run: bool,
}

impl SeatTools {
    pub const NONE: SeatTools = SeatTools {
        web: false,
        read: false,
        write: false,
        run: false,
    };

    pub fn any(self) -> bool {
        self.web || self.read || self.write || self.run
    }
}

/// The exact tool grant for this turn, as the model must understand it
/// (EXPERIMENT.md §7). `None` when the turn has no tools at all.
pub fn tool_context(tools: SeatTools, workspace: Option<&str>, artifacts: &str) -> Option<String> {
    if !tools.any() {
        return None;
    }
    let mut granted = Vec::new();
    let mut denied = Vec::new();
    for (on, name) in [
        (tools.web, "web search"),
        (
            tools.read,
            "reading and searching the workspace and artifacts folders",
        ),
        (tools.write, "creating files inside the artifacts folder"),
        (tools.run, "running code inside the artifacts folder"),
    ] {
        if on {
            granted.push(name);
        } else {
            denied.push(name);
        }
    }
    let folders = if tools.read || tools.write || tools.run {
        let workspace_line = workspace
            .map(|path| format!(" Workspace (read-only): {path}."))
            .unwrap_or_default();
        format!("{workspace_line} Artifacts (writable): {artifacts}.")
    } else {
        String::new()
    };
    let denied_line = if denied.is_empty() {
        String::new()
    } else {
        format!(" Not granted this turn: {}.", denied.join("; "))
    };
    Some(format!(
        " This turn grants you these tools and no others: {}.{denied_line}{folders} \
Cite the URLs and file paths you used (rule 9); create or run only what the human asked for \
(rule 10). Never claim to have used a tool you were not granted.",
        granted.join("; ")
    ))
}

pub fn speaking_instructions_with(
    agent: AgentId,
    language: Option<&str>,
    tools: Option<&str>,
) -> String {
    format!(
        "{}{}",
        speaking_instructions_in(agent, language),
        tools.unwrap_or_default()
    )
}

pub fn speaking_instructions_in(agent: AgentId, language: Option<&str>) -> String {
    let directive = language
        .and_then(language_name)
        .map(|name| {
            format!(" Speak in {name} in the room, regardless of the language other participants or the human used.")
        })
        .unwrap_or_default();
    format!(
        "{COUNCIL_RULES}\nYou are {agent}. The non-semantic floor controller granted you the floor. \
Write only the contribution that should become the next public Room Event. Do not mention hidden \
reasoning, the floor, or the decision protocol. Normally use 1-4 sentences.{directive}"
    )
}

pub fn speaking_input(room: &RoomSnapshot) -> String {
    format!(
        "Complete authoritative Council Room transcript:\n\n{}\n\nSpeak now with one concise, useful contribution.",
        transcript(room)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: SeatTools = SeatTools {
        web: true,
        read: true,
        write: true,
        run: true,
    };

    #[test]
    fn known_language_codes_map_to_english_names() {
        assert_eq!(language_name("ko"), Some("Korean"));
        assert_eq!(language_name("en"), Some("English"));
        assert_eq!(language_name("ja"), Some("Japanese"));
        assert_eq!(language_name("zh"), Some("Chinese"));
        assert_eq!(language_name("xx"), None);
    }

    #[test]
    fn a_language_choice_becomes_a_speech_directive() {
        let with = speaking_instructions_in(AgentId::Claude, Some("ja"));
        assert!(with.contains("Speak in Japanese"));

        // 자동 — no directive, the model follows the room.
        let without = speaking_instructions_in(AgentId::Claude, None);
        assert!(!without.contains("Speak in"));
        assert_eq!(without, speaking_instructions(AgentId::Claude));
    }

    #[test]
    fn a_full_grant_names_every_tool_and_both_folders() {
        let ctx = tool_context(FULL, Some("/Users/x/proj"), "/tmp/artifacts-7").expect("ctx");
        assert!(ctx.contains("web search; reading and searching"));
        assert!(ctx.contains("running code inside the artifacts folder"));
        assert!(!ctx.contains("Not granted"));
        assert!(ctx.contains("Workspace (read-only): /Users/x/proj"));
        assert!(ctx.contains("Artifacts (writable): /tmp/artifacts-7"));

        let speak = speaking_instructions_with(AgentId::Gpt, Some("en"), Some(&ctx));
        assert!(speak.contains("Speak in English"));
        assert!(speak.contains("Artifacts (writable): /tmp/artifacts-7"));
    }

    #[test]
    fn a_partial_grant_lists_what_is_withheld() {
        let read_only = SeatTools {
            web: true,
            read: true,
            write: false,
            run: false,
        };
        let ctx = tool_context(read_only, Some("/Users/x/proj"), "/tmp/a").expect("ctx");
        assert!(ctx.contains(
            "Not granted this turn: creating files inside the artifacts folder; running code"
        ));
        assert!(ctx.contains("Workspace (read-only): /Users/x/proj"));
    }

    #[test]
    fn a_web_only_grant_names_no_folders() {
        let web = SeatTools {
            web: true,
            ..SeatTools::NONE
        };
        let ctx = tool_context(web, Some("/Users/x/proj"), "/tmp/a").expect("ctx");
        assert!(ctx.contains("these tools and no others: web search."));
        assert!(!ctx.contains("Artifacts (writable)"));
        assert!(ctx.contains("Not granted this turn: reading"));
    }

    #[test]
    fn no_grant_means_no_tool_talk_at_all() {
        assert_eq!(tool_context(SeatTools::NONE, None, "/tmp/a"), None);
        let speak = speaking_instructions_with(AgentId::Gpt, None, None);
        assert_eq!(speak, speaking_instructions(AgentId::Gpt));
        assert!(!speak.contains("Artifacts"));
    }

    #[test]
    fn a_judgement_turn_is_told_it_has_no_tools() {
        let judge = evaluation_instructions(AgentId::Gemini);
        assert!(judge.contains("This judgement turn grants no tools"));
    }

    #[test]
    fn an_unknown_code_adds_no_directive_instead_of_gibberish() {
        let odd = speaking_instructions_in(AgentId::Grok, Some("xx"));
        assert_eq!(odd, speaking_instructions(AgentId::Grok));
    }
}
