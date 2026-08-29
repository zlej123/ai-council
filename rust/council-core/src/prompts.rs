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
a short internal reason and must not contain a proposed speech. Do not answer the room yet."
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
    fn an_unknown_code_adds_no_directive_instead_of_gibberish() {
        let odd = speaking_instructions_in(AgentId::Grok, Some("xx"));
        assert_eq!(odd, speaking_instructions(AgentId::Grok));
    }
}
