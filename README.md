# Granola Local

A local-first take on [Granola](https://granola.ai) for Windows. It records your meetings, transcribes them, and turns your rough notes into clean meeting notes.

Unlike Granola, nothing goes to a Granola server. Every meeting is a **plain markdown file on your disk**. Audio goes only to the AI provider you configure (OpenAI by default), using **your own API key**.

## Features

- **Records both sides of a call** with no virtual audio driver:
  - your microphone is labelled **Me**
  - computer audio (Zoom, Teams, Meet, …) is captured through Windows WASAPI loopback and labelled **Them**
- **Live transcript** using OpenAI `gpt-4o-transcribe`. You can switch to `gpt-4o-mini-transcribe` or `whisper-1`. Audio is cut at natural pauses so words aren't split, and silence is never uploaded.
- **AI notes**:
  - Jot short notes during the meeting. When you press Stop, the AI expands them into structured notes from the transcript, and gives untitled meetings a title.
  - Templates: General, 1:1, Stand-up, Customer call, Interview. Templates are markdown files, so you can edit them or add your own.
- **Ask questions** about one meeting, or across all your meetings.
- **Plain markdown storage**. Point the notes folder at an Obsidian vault or a synced folder if you like. Search, rename and trash all work on the files directly.
- **API key in Windows Credential Manager**. The key is never written to your notes or to the settings file.
- **Works with any OpenAI-compatible server** if you change the base URL (Azure-style proxies, local servers such as LM Studio or Ollama for chat).

## Install (Windows 10/11)

Download the `.exe` (NSIS) or `.msi` installer from the latest GitHub Actions run (**Actions → Windows build → artifacts**) or from a tagged Release. WebView2 is installed automatically if it's missing.

On first launch, Settings opens. Paste your OpenAI API key (from platform.openai.com → API keys), press **Test connection**, then save.

Windows may ask for microphone permission. If recording fails, enable **Settings → Privacy & security → Microphone → Let desktop apps access your microphone**.

## Using it

1. Press **Start a meeting** (or Ctrl+N, then **Record**).
2. Type short notes as the meeting happens. Open **Transcript** to watch it fill in.
3. Press **Stop**. AI notes are generated automatically. To regenerate with a different template, pick it and press **Generate notes** (Ctrl+Enter). **Undo** restores the previous version.
4. Ask follow-ups in the bar at the bottom, or use **Ask all meetings** in the sidebar.

Shortcuts: Ctrl+N new meeting · Ctrl+K search · Ctrl+Enter generate notes · Ctrl+S save now · Tab/Shift+Tab indent in notes.

**Tip:** wear headphones. With speakers, your mic also picks up the other side, and their words can appear twice.

## Where things live

| What | Where |
| --- | --- |
| Meetings | `Documents\Granola Local\*.md` (changeable in Settings) |
| Templates | `Documents\Granola Local\_templates\*.md` |
| Deleted meetings | `Documents\Granola Local\_trash\` |
| Settings | `%APPDATA%\GranolaLocal\settings.json` |
| API key | Windows Credential Manager → `GranolaLocal` (or the `OPENAI_API_KEY` env var) |

A meeting file looks like this:

```markdown
---
id: "20261013-143012-1a2b"
title: "Weekly sync"
created: "2026-10-13T14:30:12+01:00"
duration_secs: 1834
attendees: ["Sam", "Priya"]
template: "general"
app: granola-local
---

# Weekly sync

<!-- granola-local:ai-notes -->
## AI Notes
### Summary
…
<!-- granola-local:my-notes -->
## My Notes
- pricing?
<!-- granola-local:transcript -->
## Transcript

**[00:00:05] Me:** Okay, let's get started.

**[00:00:09] Them:** First up is pricing.
```

You can edit files in any editor. The `<!-- granola-local:… -->` markers keep the sections apart and don't show up in rendered markdown. A plain `.md` file dropped into the folder appears as a meeting, with its contents shown as your notes.

## Why Rust + Tauri

You asked for Rust, and Tauri 2 is the strongest option for this on Windows:

- **Small installer (~5 MB)** and low memory use. It uses the WebView2 built into Windows instead of shipping Chromium like Electron does.
- **Native audio**: [`cpal`](https://github.com/RustAudio/cpal) gives direct WASAPI access, including loopback capture of system audio. Electron or .NET would need extra native modules for the same thing.
- **Native secret storage** through the [`keyring`](https://crates.io/crates/keyring) crate (Windows Credential Manager).

The UI is plain HTML/CSS/JS with no bundler or CDN, so the app works fully offline apart from the AI calls.

## Development

Requirements: Rust (stable), Node 18+, and on Windows the "Desktop development with C++" build tools.

```bash
npm install
npm run dev          # run the app with hot reload of the UI
cd src-tauri && cargo test
npm run build        # build installers for the current OS
```

The app also runs on macOS for development (Keychain is used for the key). System-audio capture is Windows-only, so on a Mac it records just the microphone.

### Layout

```
src/                 UI (index.html, styles.css, main.js)
src-tauri/src/
  lib.rs             Tauri commands + app state
  notes.rs           markdown file format, list/search/save/trash, templates
  recorder.rs        recording session: chunking, transcription, appending segments
  audio.rs           mic + WASAPI loopback capture, resampling, WAV encoding
  openai.rs          transcription, streaming chat, model listing
  prompts.rs         prompts for notes, titles and Q&A
  settings.rs        settings file + credential-store API key
.github/workflows/windows.yml   builds and tests the Windows installers
```

### Releasing

Push a tag such as `v0.1.0`. The workflow builds the NSIS and MSI installers and attaches them to a GitHub Release. The installers aren't code-signed, so SmartScreen will warn on first run until you add a signing certificate.
