//! Minimal OpenAI-compatible client: audio transcription + chat completions.
//! Works with api.openai.com or any compatible server (set a custom base URL).

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct AiConfig {
    pub base_url: String,
    pub api_key: String,
    pub chat_model: String,
    pub transcription_model: String,
    pub language: String,
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .expect("http client")
}

fn url(cfg: &AiConfig, path: &str) -> String {
    format!("{}/{}", cfg.base_url.trim_end_matches('/'), path)
}

async fn error_message(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(300).collect());
    match status.as_u16() {
        401 => format!("OpenAI rejected the API key (401). Check it in Settings. {detail}"),
        429 => format!("Rate limited or out of credit (429): {detail}"),
        _ => format!("OpenAI error {status}: {detail}"),
    }
}

/// Transcribe one WAV chunk. `prompt` gives the model recent context so names
/// and terms stay consistent across chunks. Retries transient failures.
pub async fn transcribe(http: &reqwest::Client, cfg: &AiConfig, wav: Vec<u8>, prompt: &str) -> Result<String> {
    let mut last_err = anyhow!("transcription failed");
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(1500 * 2u64.pow(attempt - 1))).await;
        }
        let part = reqwest::multipart::Part::bytes(wav.clone())
            .file_name("chunk.wav")
            .mime_str("audio/wav")?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", cfg.transcription_model.clone())
            .text("response_format", "json");
        if !cfg.language.trim().is_empty() {
            form = form.text("language", cfg.language.trim().to_string());
        }
        if !prompt.trim().is_empty() {
            form = form.text("prompt", prompt.to_string());
        }
        let resp = http
            .post(url(cfg, "audio/transcriptions"))
            .bearer_auth(&cfg.api_key)
            .timeout(Duration::from_secs(120))
            .multipart(form)
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let v: Value = r.json().await.context("parsing transcription response")?;
                return Ok(v["text"].as_str().unwrap_or_default().trim().to_string());
            }
            Ok(r) => {
                let retryable = r.status().as_u16() == 429 || r.status().is_server_error();
                last_err = anyhow!(error_message(r).await);
                if !retryable {
                    break;
                }
            }
            Err(e) => last_err = anyhow!("Network error contacting OpenAI: {e}"),
        }
    }
    Err(last_err)
}

/// Chat completion, streamed through `on_delta`. Falls back to a single
/// non-streamed response if the server refuses streaming (some OpenAI models
/// require a verified organization to stream).
pub async fn chat(
    http: &reqwest::Client,
    cfg: &AiConfig,
    messages: Vec<Value>,
    mut on_delta: impl FnMut(&str),
) -> Result<String> {
    let body = json!({ "model": cfg.chat_model, "messages": messages, "stream": true });
    let resp = http
        .post(url(cfg, "chat/completions"))
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("Network error contacting OpenAI: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let msg = error_message(resp).await;
        if status.as_u16() == 400 && msg.to_lowercase().contains("stream") {
            let text = chat_once(http, cfg, messages).await?;
            on_delta(&text);
            return Ok(text);
        }
        bail!(msg);
    }

    let mut full = String::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow!("Connection dropped while streaming: {e}"))?;
        pending.extend_from_slice(&chunk);
        // Split on whole lines only, so multi-byte UTF-8 never gets cut in half.
        while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = pending.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(full);
            }
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if let Some(err) = v["error"]["message"].as_str() {
                    bail!("OpenAI error: {err}");
                }
                if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
                    if !delta.is_empty() {
                        full.push_str(delta);
                        on_delta(delta);
                    }
                }
            }
        }
    }
    Ok(full)
}

pub async fn chat_once(http: &reqwest::Client, cfg: &AiConfig, messages: Vec<Value>) -> Result<String> {
    let resp = http
        .post(url(cfg, "chat/completions"))
        .bearer_auth(&cfg.api_key)
        .json(&json!({ "model": cfg.chat_model, "messages": messages }))
        .send()
        .await
        .map_err(|e| anyhow!("Network error contacting OpenAI: {e}"))?;
    if !resp.status().is_success() {
        bail!(error_message(resp).await);
    }
    let v: Value = resp.json().await?;
    Ok(v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string())
}

pub async fn list_models(http: &reqwest::Client, cfg: &AiConfig) -> Result<Vec<String>> {
    let resp = http
        .get(url(cfg, "models"))
        .bearer_auth(&cfg.api_key)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| anyhow!("Network error contacting {}: {e}", cfg.base_url))?;
    if !resp.status().is_success() {
        bail!(error_message(resp).await);
    }
    let v: Value = resp.json().await?;
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}
