//! Prompt construction for note enhancement and Q&A.

use crate::notes::{format_ts, Meeting, Template};
use crate::settings::Settings;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub fn transcript_text(m: &Meeting) -> String {
    m.transcript
        .iter()
        .map(|s| format!("[{}] {}: {}", format_ts(s.start), s.speaker, s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn meeting_context(m: &Meeting) -> String {
    let date = chrono::DateTime::parse_from_rfc3339(&m.created)
        .map(|d| d.format("%A %-d %B %Y, %H:%M").to_string())
        .unwrap_or_else(|_| m.created.clone());
    let mut s = format!("Meeting title: {}\nDate: {}\n", if m.title.trim().is_empty() { "(untitled)" } else { m.title.trim() }, date);
    if !m.attendees.is_empty() {
        s.push_str(&format!("Attendees: {}\n", m.attendees.join(", ")));
    }
    s.push_str(&format!(
        "\n<my_notes>\n{}\n</my_notes>\n\n<transcript>\n{}\n</transcript>\n",
        if m.my_notes.trim().is_empty() { "(none)" } else { m.my_notes.trim() },
        if m.transcript.is_empty() { "(no transcript)".to_string() } else { transcript_text(m) }
    ));
    s
}

pub fn enhance(m: &Meeting, template: &Template, settings: &Settings) -> Vec<Value> {
    let system = format!(
        "You turn a person's rough meeting notes and a raw meeting transcript into polished, accurate meeting notes.\n\n\
Rules:\n\
- The person's own notes show what they care about. Keep every point they wrote, expand each with relevant detail from the transcript, and follow their structure where it makes sense.\n\
- Only state what the transcript or notes support. Never invent names, numbers, dates or commitments. If something is unclear, say so briefly.\n\
- The transcript is machine-generated and may contain errors. \"{me}\" is the person taking notes (their microphone). \"{them}\" is everyone else on the call (computer audio) and may be several different people; use names only when the conversation makes them clear.\n\
- Write concise Markdown: `###` headings, short bullets, **bold** for key facts. No preamble, no closing remarks, no top-level `#` title, and don't wrap the output in a code block.\n\n\
Template — {name}:\n{instructions}{custom}",
        me = settings.my_name,
        them = settings.their_name,
        name = template.name,
        instructions = template.instructions,
        custom = if settings.custom_instructions.trim().is_empty() {
            String::new()
        } else {
            format!("\n\nAdditional instructions from the user:\n{}", settings.custom_instructions.trim())
        }
    );
    vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": meeting_context(m)})]
}

pub fn title(m: &Meeting, notes: &str) -> Vec<Value> {
    let excerpt: String = notes.chars().take(4000).collect();
    vec![
        json!({"role": "system", "content": "Write a short, specific title (max 7 words) for this meeting. Reply with the title only — no quotes, no trailing punctuation."}),
        json!({"role": "user", "content": format!("Attendees: {}\n\n{}", m.attendees.join(", "), excerpt)}),
    ]
}

pub fn ask(m: &Meeting, history: &[ChatMessage], settings: &Settings) -> Vec<Value> {
    let system = format!(
        "You answer questions about one meeting using its notes and transcript below. \"{}\" is the user; \"{}\" is the other participants. \
Be concise and specific, quote the transcript when helpful, and say plainly when the answer isn't in the material. Use Markdown.\n\n{}{}",
        settings.my_name,
        settings.their_name,
        meeting_context(m),
        if m.enhanced_notes.trim().is_empty() { String::new() } else { format!("\n<ai_notes>\n{}\n</ai_notes>\n", m.enhanced_notes.trim()) }
    );
    with_history(system, history)
}

/// Q&A across many meetings; `corpus` is pre-formatted per-meeting context.
pub fn ask_all(corpus: &str, history: &[ChatMessage], settings: &Settings) -> Vec<Value> {
    let today = chrono::Local::now().format("%A %-d %B %Y").to_string();
    let system = format!(
        "You answer questions across the user's meeting notes below (most recent first). Today is {today}. \"{}\" is the user. \
Cite which meeting (title and date) each point comes from. Be concise, and say plainly when the notes don't cover something. Use Markdown.\n\n{corpus}",
        settings.my_name
    );
    with_history(system, history)
}

fn with_history(system: String, history: &[ChatMessage]) -> Vec<Value> {
    let mut msgs = vec![json!({"role": "system", "content": system})];
    for h in history.iter().rev().take(20).rev() {
        let role = if h.role == "assistant" { "assistant" } else { "user" };
        msgs.push(json!({"role": role, "content": h.content}));
    }
    msgs
}

pub fn corpus_entry(m: &Meeting, budget: usize) -> String {
    let date = chrono::DateTime::parse_from_rfc3339(&m.created)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| m.created.clone());
    let body = if !m.enhanced_notes.trim().is_empty() {
        format!("{}\n\nMy notes:\n{}", m.enhanced_notes.trim(), m.my_notes.trim())
    } else if !m.my_notes.trim().is_empty() || !m.transcript.is_empty() {
        format!("My notes:\n{}\n\nTranscript:\n{}", m.my_notes.trim(), transcript_text(m))
    } else {
        String::new()
    };
    let body: String = body.chars().take(budget).collect();
    format!("<meeting title=\"{}\" date=\"{}\">\n{}\n</meeting>\n", m.title.replace('"', "'"), date, body)
}
