mod audio;
mod notes;
mod openai;
mod prompts;
mod recorder;
mod settings;

use notes::{Meeting, MeetingPatch, MeetingSummary, Template};
use prompts::ChatMessage;
use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn anyerr(e: anyhow::Error) -> String {
    format!("{e:#}")
}

pub struct AppState {
    settings: Mutex<Settings>,
    /// Serialises read-modify-write of meeting files between the UI and the recorder.
    file_lock: Arc<Mutex<()>>,
    http: reqwest::Client,
    recorder: tokio::sync::Mutex<Option<recorder::Session>>,
}

impl AppState {
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }
    fn notes_dir(&self) -> PathBuf {
        self.settings().notes_path()
    }
    fn ai(&self) -> CmdResult<openai::AiConfig> {
        let s = self.settings();
        let key = settings::api_key().ok_or("Add your OpenAI API key in Settings to use AI features.")?;
        Ok(openai::AiConfig {
            base_url: s.base_url,
            api_key: key,
            chat_model: s.chat_model,
            transcription_model: s.transcription_model,
            language: s.language,
        })
    }
}

// ---------- settings ----------

#[derive(Serialize)]
struct SettingsView {
    settings: Settings,
    has_api_key: bool,
    api_key_hint: String,
    platform: &'static str,
    system_audio_supported: bool,
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> SettingsView {
    let key = settings::api_key();
    SettingsView {
        settings: state.settings(),
        has_api_key: key.is_some(),
        api_key_hint: key.as_deref().map(settings::mask_key).unwrap_or_default(),
        platform: std::env::consts::OS,
        system_audio_supported: cfg!(windows),
    }
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: Settings) -> CmdResult<SettingsView> {
    let s = settings.sanitized();
    std::fs::create_dir_all(s.notes_path()).map_err(|e| format!("Can't use that notes folder: {e}"))?;
    settings::save(&s).map_err(anyerr)?;
    *state.settings.lock().unwrap() = s;
    Ok(get_settings(state))
}

#[tauri::command]
fn set_api_key(state: State<AppState>, key: String) -> CmdResult<SettingsView> {
    settings::set_api_key(&key).map_err(anyerr)?;
    Ok(get_settings(state))
}

#[tauri::command]
async fn test_connection(state: State<'_, AppState>) -> CmdResult<String> {
    let ai = state.ai()?;
    let models = openai::list_models(&state.http, &ai).await.map_err(anyerr)?;
    let has = |m: &str| models.is_empty() || models.iter().any(|x| x == m);
    let mut msg = format!("Connected — {} models available.", models.len());
    if !has(&ai.chat_model) {
        msg.push_str(&format!(" ⚠ Notes model “{}” isn't in your model list.", ai.chat_model));
    }
    if !has(&ai.transcription_model) {
        msg.push_str(&format!(" ⚠ Transcription model “{}” isn't in your model list.", ai.transcription_model));
    }
    Ok(msg)
}

#[tauri::command]
fn list_input_devices() -> Vec<String> {
    audio::input_device_names()
}

// ---------- meetings ----------

#[tauri::command]
fn list_meetings(state: State<AppState>, query: Option<String>) -> Vec<MeetingSummary> {
    notes::list(&state.notes_dir(), query.as_deref().unwrap_or(""))
}

#[tauri::command]
fn get_meeting(state: State<AppState>, id: String) -> CmdResult<Meeting> {
    let _g = state.file_lock.lock().unwrap();
    notes::load(&state.notes_dir(), &id).map_err(anyerr)
}

#[tauri::command]
fn create_meeting(state: State<AppState>, title: Option<String>) -> CmdResult<Meeting> {
    let s = state.settings();
    let _g = state.file_lock.lock().unwrap();
    notes::create(&s.notes_path(), title.as_deref().unwrap_or(""), &s.default_template).map_err(anyerr)
}

#[tauri::command]
fn update_meeting(state: State<AppState>, id: String, patch: MeetingPatch) -> CmdResult<Meeting> {
    let dir = state.notes_dir();
    let _g = state.file_lock.lock().unwrap();
    let mut m = notes::load(&dir, &id).map_err(anyerr)?;
    notes::apply_patch(&mut m, patch);
    notes::save(&dir, &mut m).map_err(anyerr)?;
    Ok(m)
}

#[tauri::command]
async fn delete_meeting(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    if let Some(s) = state.recorder.lock().await.as_ref() {
        if s.meeting_id == id {
            return Err("Stop recording before deleting this meeting.".into());
        }
    }
    let _g = state.file_lock.lock().unwrap();
    notes::trash(&state.notes_dir(), &id).map_err(anyerr)
}

#[tauri::command]
fn list_templates(state: State<AppState>) -> Vec<Template> {
    notes::list_templates(&state.notes_dir())
}

#[tauri::command]
fn open_path(state: State<AppState>, which: String, id: Option<String>) -> CmdResult<()> {
    let dir = state.notes_dir();
    std::fs::create_dir_all(&dir).map_err(err)?;
    let target = match which.as_str() {
        "templates" => {
            notes::ensure_templates(&dir).map_err(anyerr)?;
            notes::templates_dir(&dir)
        }
        "meeting" => {
            let _g = state.file_lock.lock().unwrap();
            id.and_then(|id| notes::find_path(&dir, &id)).ok_or("Meeting file not found")?
        }
        _ => dir,
    };
    reveal(&target).map_err(err)
}

fn reveal(path: &std::path::Path) -> std::io::Result<()> {
    let mut cmd;
    if cfg!(windows) {
        cmd = std::process::Command::new("explorer");
        if path.is_file() {
            cmd.arg(format!("/select,{}", path.display()));
        } else {
            cmd.arg(path);
        }
    } else if cfg!(target_os = "macos") {
        cmd = std::process::Command::new("open");
        if path.is_file() {
            cmd.arg("-R");
        }
        cmd.arg(path);
    } else {
        cmd = std::process::Command::new("xdg-open");
        cmd.arg(if path.is_file() { path.parent().unwrap_or(path) } else { path });
    }
    cmd.spawn().map(|_| ())
}

// ---------- recording ----------

#[tauri::command]
async fn start_recording(app: AppHandle, state: State<'_, AppState>, meeting_id: String) -> CmdResult<recorder::Status> {
    let mut slot = state.recorder.lock().await;
    if let Some(s) = slot.as_ref() {
        return Err(format!("Already recording another meeting ({}).", s.meeting_id));
    }
    let ai = state.ai()?;
    let s = state.settings();
    let args = recorder::StartArgs {
        app,
        http: state.http.clone(),
        ai,
        notes_dir: s.notes_path(),
        file_lock: state.file_lock.clone(),
        meeting_id,
        mic_device: s.mic_device.clone(),
        capture_system: s.capture_system_audio,
        my_name: s.my_name.clone(),
        their_name: s.their_name.clone(),
        chunk_secs: s.chunk_seconds,
    };
    // Opening audio devices blocks briefly; keep it off the async runtime.
    let (session, warnings) = tauri::async_runtime::spawn_blocking(move || recorder::Session::start(args))
        .await
        .map_err(err)?
        .map_err(anyerr)?;
    let status = session.status(warnings);
    *slot = Some(session);
    Ok(status)
}

#[tauri::command]
async fn stop_recording(state: State<'_, AppState>) -> CmdResult<Option<Meeting>> {
    let session = state.recorder.lock().await.take();
    match session {
        Some(s) => s.stop().await.map(Some).map_err(anyerr),
        None => Ok(None),
    }
}

#[tauri::command]
async fn recording_status(state: State<'_, AppState>) -> CmdResult<Option<recorder::Status>> {
    Ok(state.recorder.lock().await.as_ref().map(|s| s.status(vec![])))
}

// ---------- AI ----------

#[derive(Serialize, Clone)]
struct Delta {
    request_id: String,
    delta: String,
}

#[tauri::command]
async fn enhance_notes(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    template_id: String,
    request_id: String,
) -> CmdResult<Meeting> {
    let ai = state.ai()?;
    let s = state.settings();
    let dir = s.notes_path();
    let meeting = {
        let _g = state.file_lock.lock().unwrap();
        notes::load(&dir, &meeting_id).map_err(anyerr)?
    };
    if meeting.transcript.is_empty() && meeting.my_notes.trim().is_empty() {
        return Err("Nothing to work with yet — record the meeting or jot some notes first.".into());
    }
    let template = notes::get_template(&dir, &template_id);
    let msgs = prompts::enhance(&meeting, &template, &s);
    let text = openai::chat(&state.http, &ai, msgs, |d| {
        let _ = app.emit("ai-delta", Delta { request_id: request_id.clone(), delta: d.to_string() });
    })
    .await
    .map_err(anyerr)?;
    let text = strip_code_fence(&text);

    let new_title = if meeting.title.trim().is_empty() {
        openai::chat_once(&state.http, &ai, prompts::title(&meeting, &text))
            .await
            .ok()
            .map(|t| t.trim().trim_matches(['"', '\'', '.']).to_string())
            .filter(|t| !t.is_empty() && t.len() < 120)
    } else {
        None
    };

    let _g = state.file_lock.lock().unwrap();
    let mut m = notes::load(&dir, &meeting_id).map_err(anyerr)?;
    m.enhanced_notes = text;
    m.template = template.id;
    if let Some(t) = new_title {
        if m.title.trim().is_empty() {
            m.title = t;
        }
    }
    notes::save(&dir, &mut m).map_err(anyerr)?;
    Ok(m)
}

fn strip_code_fence(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
        return rest.trim_end().trim_end_matches("```").trim().to_string();
    }
    t.to_string()
}

#[tauri::command]
async fn ask(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: Option<String>,
    messages: Vec<ChatMessage>,
    request_id: String,
) -> CmdResult<String> {
    let ai = state.ai()?;
    let s = state.settings();
    let dir = s.notes_path();
    let msgs = match meeting_id {
        Some(id) => {
            let m = {
                let _g = state.file_lock.lock().unwrap();
                notes::load(&dir, &id).map_err(anyerr)?
            };
            prompts::ask(&m, &messages, &s)
        }
        None => {
            // Recent meetings, newest first, within a generous context budget.
            let mut corpus = String::new();
            let summaries = notes::list(&dir, "");
            let _g = state.file_lock.lock().unwrap();
            for sum in summaries.iter().take(60) {
                if corpus.len() > 300_000 {
                    break;
                }
                if let Ok(m) = notes::load(&dir, &sum.id) {
                    corpus.push_str(&prompts::corpus_entry(&m, 12_000));
                }
            }
            prompts::ask_all(&corpus, &messages, &s)
        }
    };
    openai::chat(&state.http, &ai, msgs, |d| {
        let _ = app.emit("ai-delta", Delta { request_id: request_id.clone(), delta: d.to_string() });
    })
    .await
    .map_err(anyerr)
}

#[tauri::command]
fn render_markdown(md: String) -> String {
    use pulldown_cmark::{html, Event, Options, Parser};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // Raw HTML is shown as text, never executed.
    let parser = Parser::new_ext(&md, opts).map(|e| match e {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let s = settings::load();
    let _ = std::fs::create_dir_all(s.notes_path());
    let _ = notes::ensure_templates(&s.notes_path());

    tauri::Builder::default()
        .manage(AppState {
            settings: Mutex::new(s),
            file_lock: Arc::new(Mutex::new(())),
            http: openai::client(),
            recorder: tokio::sync::Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            set_api_key,
            test_connection,
            list_input_devices,
            list_meetings,
            get_meeting,
            create_meeting,
            update_meeting,
            delete_meeting,
            list_templates,
            open_path,
            start_recording,
            stop_recording,
            recording_status,
            enhance_notes,
            ask,
            render_markdown,
        ])
        .on_window_event(|window, event| {
            // Finish transcribing an in-progress recording before the app exits.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle().clone();
                let recording = app
                    .state::<AppState>()
                    .recorder
                    .try_lock()
                    .map(|g| g.is_some())
                    .unwrap_or(true);
                if recording {
                    api.prevent_close();
                    let _ = app.emit("app-closing", ());
                    tauri::async_runtime::spawn(async move {
                        let session = app.state::<AppState>().recorder.lock().await.take();
                        if let Some(s) = session {
                            let _ = s.stop().await;
                        }
                        app.exit(0);
                    });
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Granola Local");
}

#[cfg(test)]
mod tests {
    #[test]
    fn markdown_escapes_html() {
        let out = super::render_markdown("hi <script>alert(1)</script>\n\n- [ ] task".into());
        assert!(!out.contains("<script>"));
        assert!(out.contains("checkbox"));
    }

    #[test]
    fn strips_fences() {
        assert_eq!(super::strip_code_fence("```markdown\n### A\n- b\n```"), "### A\n- b");
        assert_eq!(super::strip_code_fence("### A"), "### A");
    }
}
