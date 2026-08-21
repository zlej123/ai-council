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
    format!(
        "{COUNCIL_RULES}\nYou are {agent}. The non-semantic floor controller granted you the floor. \
Write only the contribution that should become the next public Room Event. Do not mention hidden \
reasoning, the floor, or the decision protocol. Normally use 1-4 sentences."
    )
}

pub fn speaking_input(room: &RoomSnapshot) -> String {
    format!(
        "Complete authoritative Council Room transcript:\n\n{}\n\nSpeak now with one concise, useful contribution.",
        transcript(room)
    )
}
