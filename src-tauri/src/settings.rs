//! App settings (plain JSON in the user's config dir) and the OpenAI API key
//! (kept in the OS credential store: Windows Credential Manager / macOS Keychain).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "GranolaLocal";
const KEYRING_USER: &str = "openai_api_key";

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Folder where meeting markdown files live.
    pub notes_dir: String,
    /// OpenAI-compatible API root, e.g. https://api.openai.com/v1
    pub base_url: String,
    pub transcription_model: String,
    pub chat_model: String,
    /// ISO-639-1 hint for transcription ("" = auto-detect).
    pub language: String,
    /// Label for the microphone speaker in transcripts.
    pub my_name: String,
    /// Label for system-audio speakers in transcripts.
    pub their_name: String,
    /// Target length of each audio chunk sent for transcription.
    pub chunk_seconds: u32,
    /// Capture what the computer plays (WASAPI loopback on Windows).
    pub capture_system_audio: bool,
    /// Microphone device name ("" = system default).
    pub mic_device: String,
    /// Extra instructions appended to every AI notes request.
    pub custom_instructions: String,
    pub default_template: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notes_dir: default_notes_dir().to_string_lossy().into_owned(),
            base_url: "https://api.openai.com/v1".into(),
            transcription_model: "gpt-4o-transcribe".into(),
            chat_model: "gpt-4.1".into(),
            language: String::new(),
            my_name: "Me".into(),
            their_name: "Them".into(),
            chunk_seconds: 20,
            capture_system_audio: true,
            mic_device: String::new(),
            custom_instructions: String::new(),
            default_template: "general".into(),
        }
    }
}

impl Settings {
    pub fn notes_path(&self) -> PathBuf {
        if self.notes_dir.trim().is_empty() {
            default_notes_dir()
        } else {
            PathBuf::from(self.notes_dir.trim())
        }
    }

    /// Clamp/normalise values coming from the UI.
    pub fn sanitized(mut self) -> Self {
        let d = Settings::default();
        self.chunk_seconds = self.chunk_seconds.clamp(8, 120);
        if self.base_url.trim().is_empty() {
            self.base_url = d.base_url;
        }
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if self.transcription_model.trim().is_empty() {
            self.transcription_model = d.transcription_model;
        }
        if self.chat_model.trim().is_empty() {
            self.chat_model = d.chat_model;
        }
        if self.my_name.trim().is_empty() {
            self.my_name = d.my_name;
        }
        if self.their_name.trim().is_empty() {
            self.their_name = d.their_name;
        }
        if self.notes_dir.trim().is_empty() {
            self.notes_dir = d.notes_dir;
        }
        self
    }
}

pub fn default_notes_dir() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Granola Local")
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("GranolaLocal")
        .join("settings.json")
}

pub fn load() -> Settings {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .unwrap_or_default()
        .sanitized()
}

pub fn save(settings: &Settings) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("creating config folder")?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(settings)?)
        .with_context(|| format!("writing {}", path.display()))
}

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).context("opening credential store")
}

/// Key from the OS credential store, falling back to the OPENAI_API_KEY env var.
pub fn api_key() -> Option<String> {
    entry()
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|k| !k.trim().is_empty())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok().filter(|k| !k.trim().is_empty()))
}

pub fn set_api_key(key: &str) -> Result<()> {
    let e = entry()?;
    let key = key.trim();
    if key.is_empty() {
        match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err).context("removing API key"),
        }
    } else {
        e.set_password(key).context("saving API key to the credential store")
    }
}

pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "••••".into();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    let head: String = chars[..3].iter().collect();
    format!("{head}…{tail}")
}
