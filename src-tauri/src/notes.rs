//! Meetings stored as plain markdown files.
//!
//! File layout (`<notes_dir>/2026-10-13 1430 Weekly sync.md`):
//!
//! ```text
//! ---
//! id: "20261013-143012-1a2b"
//! title: "Weekly sync"
//! created: "2026-10-13T14:30:12+01:00"
//! duration_secs: 1834
//! attendees: ["Sam", "Priya"]
//! template: "general"
//! ---
//!
//! # Weekly sync
//!
//! <!-- granola-local:ai-notes -->
//! ## AI Notes
//! ...
//! <!-- granola-local:my-notes -->
//! ## My Notes
//! ...
//! <!-- granola-local:transcript -->
//! ## Transcript
//!
//! **[00:00:05] Me:** ...
//! ```
//!
//! Frontmatter values are JSON literals, which are also valid YAML, so the
//! files open cleanly in Obsidian and other markdown tools.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const M_AI: &str = "<!-- granola-local:ai-notes -->";
const M_MINE: &str = "<!-- granola-local:my-notes -->";
const M_TRANSCRIPT: &str = "<!-- granola-local:transcript -->";
const TRASH_DIR: &str = "_trash";
const TEMPLATES_DIR: &str = "_templates";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Segment {
    /// Seconds from the start of the meeting.
    pub start: f64,
    pub speaker: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    /// RFC 3339 local timestamp.
    pub created: String,
    pub duration_secs: u64,
    pub attendees: Vec<String>,
    pub template: String,
    pub my_notes: String,
    pub enhanced_notes: String,
    pub transcript: Vec<Segment>,
    /// Absolute path of the backing file (informational, not persisted).
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct MeetingPatch {
    pub title: Option<String>,
    pub attendees: Option<Vec<String>>,
    pub my_notes: Option<String>,
    pub enhanced_notes: Option<String>,
    pub template: Option<String>,
    pub transcript: Option<Vec<Segment>>,
}

#[derive(Serialize, Clone, Debug)]
pub struct MeetingSummary {
    pub id: String,
    pub title: String,
    pub created: String,
    pub duration_secs: u64,
    pub snippet: String,
    pub has_transcript: bool,
    pub has_ai_notes: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub instructions: String,
}

// ---------- serialisation ----------

pub fn format_ts(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn parse_ts(ts: &str) -> Option<f64> {
    let parts: Vec<&str> = ts.trim().split(':').collect();
    let nums: Option<Vec<f64>> = parts.iter().map(|p| p.parse::<f64>().ok()).collect();
    let nums = nums?;
    match nums.as_slice() {
        [h, m, s] => Some(h * 3600.0 + m * 60.0 + s),
        [m, s] => Some(m * 60.0 + s),
        _ => None,
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

pub fn serialize(m: &Meeting) -> String {
    let title = if m.title.trim().is_empty() { "Untitled meeting" } else { m.title.trim() };
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", json_str(&m.id)));
    out.push_str(&format!("title: {}\n", json_str(&m.title)));
    out.push_str(&format!("created: {}\n", json_str(&m.created)));
    out.push_str(&format!("duration_secs: {}\n", m.duration_secs));
    out.push_str(&format!(
        "attendees: {}\n",
        serde_json::to_string(&m.attendees).unwrap_or_else(|_| "[]".into())
    ));
    out.push_str(&format!("template: {}\n", json_str(&m.template)));
    out.push_str("app: granola-local\n");
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));

    out.push_str(M_AI);
    out.push_str("\n## AI Notes\n\n");
    push_block(&mut out, &m.enhanced_notes);

    out.push_str(M_MINE);
    out.push_str("\n## My Notes\n\n");
    push_block(&mut out, &m.my_notes);

    out.push_str(M_TRANSCRIPT);
    out.push_str("\n## Transcript\n\n");
    for seg in &m.transcript {
        let text = seg.text.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&format!("**[{}] {}:** {}\n\n", format_ts(seg.start), seg.speaker.trim(), text));
    }
    out
}

fn push_block(out: &mut String, body: &str) {
    let body = body.trim_matches('\n');
    if !body.trim().is_empty() {
        out.push_str(body);
        out.push_str("\n\n");
    }
}

/// Split `---` frontmatter from the body. Values are kept raw.
fn split_frontmatter(content: &str) -> (BTreeMap<String, String>, &str) {
    let mut map = BTreeMap::new();
    let content = content.trim_start_matches('\u{feff}');
    let Some(rest) = content.strip_prefix("---") else {
        return (map, content);
    };
    let rest = rest.trim_start_matches('\r').strip_prefix('\n').unwrap_or(rest);
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        let l = line.trim_end();
        if l == "---" {
            return (map, &rest[offset..]);
        }
        if let Some((k, v)) = l.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    // No closing fence: treat everything as body.
    (BTreeMap::new(), content)
}

fn fm_string(map: &BTreeMap<String, String>, key: &str) -> String {
    let Some(raw) = map.get(key) else { return String::new() };
    if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.trim_matches('"').to_string())
    } else {
        raw.trim_matches('\'').to_string()
    }
}

fn fm_list(map: &BTreeMap<String, String>, key: &str) -> Vec<String> {
    let Some(raw) = map.get(key) else { return vec![] };
    if raw.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Vec<String>>(raw) {
            return v;
        }
        return raw
            .trim_matches(|c| c == '[' || c == ']')
            .split(',')
            .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Drop a leading `## Heading` line that we write ourselves.
fn strip_heading<'a>(section: &'a str, heading: &str) -> &'a str {
    let s = section.trim_start_matches(['\r', '\n']);
    match s.strip_prefix(heading) {
        Some(rest) if rest.starts_with('\n') || rest.starts_with('\r') || rest.is_empty() => rest,
        _ => s,
    }
}

fn clean_block(s: &str) -> String {
    s.replace("\r\n", "\n").trim_matches('\n').trim_end().to_string()
}

fn parse_transcript(s: &str) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    for line in s.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let parsed = (|| {
            let rest = l.strip_prefix("**[")?;
            let (ts, rest) = rest.split_once(']')?;
            let (speaker, text) = rest.split_once(":**")?;
            Some(Segment {
                start: parse_ts(ts)?,
                speaker: speaker.trim().to_string(),
                text: text.trim().to_string(),
            })
        })();
        match parsed {
            Some(seg) => segs.push(seg),
            None => {
                if let Some(last) = segs.last_mut() {
                    last.text.push(' ');
                    last.text.push_str(l);
                }
            }
        }
    }
    segs
}

pub fn parse(content: &str) -> Meeting {
    let (fm, body) = split_frontmatter(content);
    let mut m = Meeting {
        id: fm_string(&fm, "id"),
        title: fm_string(&fm, "title"),
        created: fm_string(&fm, "created"),
        duration_secs: fm.get("duration_secs").and_then(|v| v.parse().ok()).unwrap_or(0),
        attendees: fm_list(&fm, "attendees"),
        template: fm_string(&fm, "template"),
        ..Default::default()
    };
    let body = body.replace("\r\n", "\n");

    let ai = body.find(M_AI);
    let mine = body.find(M_MINE);
    let tr = body.find(M_TRANSCRIPT);
    if ai.is_none() && mine.is_none() && tr.is_none() {
        // A file we didn't write (or hand-made): the whole body is the user's notes,
        // minus an initial H1 that just repeats the title.
        let mut b = body.trim_start();
        if let Some(rest) = b.strip_prefix("# ") {
            let (h1, after) = rest.split_once('\n').unwrap_or((rest, ""));
            if m.title.is_empty() {
                m.title = h1.trim().to_string();
            }
            b = after;
        }
        m.my_notes = clean_block(b);
        return m;
    }

    // Collect (position, kind) markers and slice the body between them.
    let mut marks: Vec<(usize, &str)> = [(ai, M_AI), (mine, M_MINE), (tr, M_TRANSCRIPT)]
        .into_iter()
        .filter_map(|(p, k)| p.map(|p| (p, k)))
        .collect();
    marks.sort();
    for (i, (pos, kind)) in marks.iter().enumerate() {
        let start = pos + kind.len();
        let end = marks.get(i + 1).map(|(p, _)| *p).unwrap_or(body.len());
        let section = &body[start..end];
        match *kind {
            M_AI => m.enhanced_notes = clean_block(strip_heading(section, "## AI Notes")),
            M_MINE => m.my_notes = clean_block(strip_heading(section, "## My Notes")),
            _ => m.transcript = parse_transcript(strip_heading(section, "## Transcript")),
        }
    }
    if !fm.contains_key("title") {
        if let Some(h1) = body.lines().find_map(|l| l.strip_prefix("# ")) {
            m.title = h1.trim().to_string();
        }
    }
    m
}

// ---------- file helpers ----------

/// Filesystem-safe (Windows included) title fragment.
pub fn slugify(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { ' ' })
        .collect();
    let mut s = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 60 {
        s = s.chars().take(60).collect::<String>().trim_end().to_string();
    }
    s
}

fn file_stem(m: &Meeting) -> String {
    let stamp = chrono::DateTime::parse_from_rfc3339(&m.created)
        .map(|d| d.format("%Y-%m-%d %H%M").to_string())
        .unwrap_or_else(|_| chrono::Local::now().format("%Y-%m-%d %H%M").to_string());
    let slug = slugify(&m.title);
    if slug.is_empty() {
        format!("{stamp} Untitled")
    } else {
        format!("{stamp} {slug}")
    }
}

fn md_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map(|x| x.eq_ignore_ascii_case("md")).unwrap_or(false))
        .collect()
}

/// Meeting id for a file: its frontmatter id, or (for hand-made files) its file stem.
fn id_for(path: &Path, m: &Meeting) -> String {
    if !m.id.is_empty() {
        return m.id.clone();
    }
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn load_file(path: &Path) -> Result<Meeting> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut m = parse(&content);
    let hand_made = m.id.is_empty();
    m.id = id_for(path, &m);
    if m.created.is_empty() {
        let modified = std::fs::metadata(path).and_then(|md| md.modified()).ok();
        let dt: chrono::DateTime<chrono::Local> = modified.map(Into::into).unwrap_or_else(chrono::Local::now);
        m.created = dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    }
    if hand_made && m.title.is_empty() {
        m.title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    m.path = path.to_string_lossy().into_owned();
    Ok(m)
}

pub fn find_path(dir: &Path, id: &str) -> Option<PathBuf> {
    // Fast path: hand-made files use their stem as id.
    let direct = dir.join(format!("{id}.md"));
    for path in md_files(dir) {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let (fm, _) = split_frontmatter(&content);
            let fid = fm_string(&fm, "id");
            if fid == id || (fid.is_empty() && path == direct) {
                return Some(path);
            }
        }
    }
    None
}

pub fn load(dir: &Path, id: &str) -> Result<Meeting> {
    let path = find_path(dir, id).ok_or_else(|| anyhow!("Meeting not found: {id}"))?;
    load_file(&path)
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

/// Write a meeting, renaming its file if the title changed. Updates `m.path`.
pub fn save(dir: &Path, m: &mut Meeting) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    if m.id.is_empty() {
        bail!("meeting has no id");
    }
    let existing = find_path(dir, &m.id);
    let stem = file_stem(m);
    let mut target = dir.join(format!("{stem}.md"));
    let mut n = 2;
    while target.exists() && Some(&target) != existing.as_ref() {
        target = dir.join(format!("{stem} ({n}).md"));
        n += 1;
    }
    write_atomic(&target, &serialize(m))?;
    if let Some(old) = existing {
        if old != target {
            let _ = std::fs::remove_file(old);
        }
    }
    m.path = target.to_string_lossy().into_owned();
    Ok(())
}

pub fn new_id() -> String {
    let now = chrono::Local::now();
    let nanos = now.timestamp_subsec_nanos();
    format!("{}-{:04x}", now.format("%Y%m%d-%H%M%S"), (nanos ^ (nanos >> 16)) & 0xffff)
}

pub fn create(dir: &Path, title: &str, template: &str) -> Result<Meeting> {
    let mut m = Meeting {
        id: new_id(),
        title: title.trim().to_string(),
        created: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        template: template.to_string(),
        ..Default::default()
    };
    save(dir, &mut m)?;
    Ok(m)
}

pub fn apply_patch(m: &mut Meeting, p: MeetingPatch) {
    if let Some(v) = p.title {
        m.title = v;
    }
    if let Some(v) = p.attendees {
        m.attendees = v.into_iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    }
    if let Some(v) = p.my_notes {
        m.my_notes = v;
    }
    if let Some(v) = p.enhanced_notes {
        m.enhanced_notes = v;
    }
    if let Some(v) = p.template {
        m.template = v;
    }
    if let Some(v) = p.transcript {
        m.transcript = v;
    }
}

pub fn insert_segment(m: &mut Meeting, seg: Segment) {
    let idx = m.transcript.partition_point(|s| s.start <= seg.start);
    m.transcript.insert(idx, seg);
}

/// Move to `<notes_dir>/_trash/` rather than deleting outright.
pub fn trash(dir: &Path, id: &str) -> Result<()> {
    let path = find_path(dir, id).ok_or_else(|| anyhow!("Meeting not found: {id}"))?;
    let trash = dir.join(TRASH_DIR);
    std::fs::create_dir_all(&trash)?;
    let name = path.file_name().ok_or_else(|| anyhow!("bad path"))?;
    let mut dest = trash.join(name);
    if dest.exists() {
        dest = trash.join(format!("{} {}", new_id(), name.to_string_lossy()));
    }
    std::fs::rename(&path, &dest).context("moving note to trash")?;
    Ok(())
}

fn snippet(m: &Meeting) -> String {
    let source = if !m.enhanced_notes.trim().is_empty() {
        m.enhanced_notes.as_str()
    } else if !m.my_notes.trim().is_empty() {
        m.my_notes.as_str()
    } else {
        m.transcript.first().map(|s| s.text.as_str()).unwrap_or("")
    };
    let flat: String = source
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ', '>']).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    flat.chars().take(140).collect()
}

pub fn list(dir: &Path, query: &str) -> Vec<MeetingSummary> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<MeetingSummary> = md_files(dir)
        .into_iter()
        .filter_map(|p| {
            let m = load_file(&p).ok()?;
            if !q.is_empty() {
                let hay = format!(
                    "{}\n{}\n{}\n{}\n{}",
                    m.title,
                    m.attendees.join(" "),
                    m.my_notes,
                    m.enhanced_notes,
                    m.transcript.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ")
                )
                .to_lowercase();
                if !q.split_whitespace().all(|w| hay.contains(w)) {
                    return None;
                }
            }
            Some(MeetingSummary {
                snippet: snippet(&m),
                has_transcript: !m.transcript.is_empty(),
                has_ai_notes: !m.enhanced_notes.trim().is_empty(),
                id: m.id,
                title: m.title,
                created: m.created,
                duration_secs: m.duration_secs,
            })
        })
        .collect();
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

// ---------- templates (also markdown files) ----------

const DEFAULT_TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "general",
        "General meeting",
        "Structure the notes as:\n\n### Summary\n2–3 sentences on what the meeting was about and what was decided.\n\n### Key points\nGrouped bullets of the substantive discussion. Use sub-headings per topic when there are several.\n\n### Decisions\n\n### Action items\n- [ ] Owner — task (due date if mentioned)",
    ),
    (
        "one-on-one",
        "1:1",
        "This is a one-on-one. Structure the notes as:\n\n### Updates\n### Wins\n### Challenges & blockers\n### Feedback\nCapture feedback given in either direction, quoting phrasing where it matters.\n### Career & growth\nOnly if discussed.\n### Action items\n- [ ] Owner — task",
    ),
    (
        "standup",
        "Stand-up",
        "This is a team stand-up. For each person who spoke, write a short sub-heading with their name (or 'Unknown speaker') and bullets for: yesterday, today, blockers. Finish with a '### Blockers to follow up' list.",
    ),
    (
        "customer-call",
        "Customer / sales call",
        "This is a call with a customer or prospect. Structure the notes as:\n\n### Context\nWho they are, their role and company, why they're talking to us.\n### Pain points\n### Current solution\n### Requirements & objections\n### Budget, timeline, decision process\nOnly what was actually said.\n### Next steps\n- [ ] Owner — task",
    ),
    (
        "interview",
        "Interview",
        "This is a job interview. Structure the notes as:\n\n### Candidate background\n### Questions & answers\nOne sub-heading per question asked, with a concise summary of the answer and notable specifics.\n### Strengths observed\n### Concerns / gaps\n### Candidate questions\nStay factual and do not make a hiring recommendation.",
    ),
];

pub fn templates_dir(dir: &Path) -> PathBuf {
    dir.join(TEMPLATES_DIR)
}

/// Creates the default template files the first time the templates folder is missing.
pub fn ensure_templates(dir: &Path) -> Result<()> {
    let tdir = templates_dir(dir);
    if tdir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&tdir)?;
    for (id, name, body) in DEFAULT_TEMPLATES {
        let content = format!("---\nname: {}\n---\n\n{}\n", json_str(name), body);
        std::fs::write(tdir.join(format!("{id}.md")), content)?;
    }
    Ok(())
}

pub fn list_templates(dir: &Path) -> Vec<Template> {
    let _ = ensure_templates(dir);
    let mut out: Vec<Template> = md_files(&templates_dir(dir))
        .into_iter()
        .filter_map(|p| {
            let content = std::fs::read_to_string(&p).ok()?;
            let (fm, body) = split_frontmatter(&content);
            let id = p.file_stem()?.to_string_lossy().into_owned();
            let name = Some(fm_string(&fm, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
            Some(Template { id, name, instructions: body.trim().to_string() })
        })
        .collect();
    if out.is_empty() {
        out = DEFAULT_TEMPLATES
            .iter()
            .map(|(id, name, body)| Template { id: id.to_string(), name: name.to_string(), instructions: body.to_string() })
            .collect();
    }
    out.sort_by_key(|t| (t.id != "general", t.name.to_lowercase()));
    out
}

pub fn get_template(dir: &Path, id: &str) -> Template {
    let all = list_templates(dir);
    all.iter()
        .find(|t| t.id == id)
        .or_else(|| all.iter().find(|t| t.id == "general"))
        .or(all.first())
        .cloned()
        .unwrap_or(Template { id: "general".into(), name: "General".into(), instructions: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Meeting {
        Meeting {
            id: "20261013-143012-1a2b".into(),
            title: "Weekly sync: Q4 \"plans\"".into(),
            created: "2026-10-13T14:30:12+01:00".into(),
            duration_secs: 1834,
            attendees: vec!["Sam".into(), "Priya O'Neil".into()],
            template: "general".into(),
            my_notes: "- budget?\n## my own heading\n- ship date".into(),
            enhanced_notes: "### Summary\nWe talked.\n\n### Action items\n- [ ] Sam — send deck".into(),
            transcript: vec![
                Segment { start: 5.0, speaker: "Me".into(), text: "Hi everyone.".into() },
                Segment { start: 3725.4, speaker: "Them".into(), text: "Thanks: see you **soon**".into() },
            ],
            path: String::new(),
        }
    }

    #[test]
    fn roundtrip() {
        let m = sample();
        let parsed = parse(&serialize(&m));
        let mut expected = m.clone();
        expected.transcript[1].start = 3725.0; // timestamps are whole seconds
        assert_eq!(parsed, expected);
    }

    #[test]
    fn roundtrip_empty_sections() {
        let m = Meeting { id: "x".into(), title: String::new(), created: "2026-01-01T00:00:00Z".into(), ..Default::default() };
        let parsed = parse(&serialize(&m));
        assert_eq!(parsed, m);
    }

    #[test]
    fn parses_crlf_files() {
        let text = serialize(&sample()).replace('\n', "\r\n");
        let parsed = parse(&text);
        assert_eq!(parsed.title, sample().title);
        assert_eq!(parsed.my_notes, sample().my_notes);
        assert_eq!(parsed.transcript.len(), 2);
    }

    #[test]
    fn plain_markdown_becomes_my_notes() {
        let m = parse("# Hand made\n\nSome *notes*\n- a\n");
        assert_eq!(m.title, "Hand made");
        assert_eq!(m.my_notes, "Some *notes*\n- a");
    }

    #[test]
    fn slug_is_windows_safe() {
        assert_eq!(slugify("a/b\\c:d*e?f\"g<h>i|j."), "a b c d e f g h i j");
        assert_eq!(slugify("  "), "");
    }

    #[test]
    fn save_list_rename_trash() {
        let dir = std::env::temp_dir().join(format!("granola-local-test-{}", new_id()));
        let mut m = create(&dir, "First title", "general").unwrap();
        assert!(Path::new(&m.path).exists());
        m.title = "Second title".into();
        m.my_notes = "hello world".into();
        save(&dir, &mut m).unwrap();
        assert!(m.path.ends_with("Second title.md"));
        assert_eq!(md_files(&dir).len(), 1);
        assert_eq!(list(&dir, "hello").len(), 1);
        assert_eq!(list(&dir, "missing").len(), 0);
        let loaded = load(&dir, &m.id).unwrap();
        assert_eq!(loaded.my_notes, "hello world");
        trash(&dir, &m.id).unwrap();
        assert_eq!(list(&dir, "").len(), 0);
        assert!(!list_templates(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn segments_insert_sorted() {
        let mut m = sample();
        insert_segment(&mut m, Segment { start: 10.0, speaker: "Them".into(), text: "x".into() });
        let starts: Vec<f64> = m.transcript.iter().map(|s| s.start).collect();
        assert_eq!(starts, vec![5.0, 10.0, 3725.4]);
    }
}
