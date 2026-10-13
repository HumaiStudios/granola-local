// Granola Local — UI. Talks to the Rust backend via Tauri commands/events.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => [...document.querySelectorAll(sel)];

const state = {
  settings: null, // SettingsView from the backend
  meetings: [],
  templates: [],
  current: null, // full Meeting currently open
  tab: "mine",
  aiEditing: false,
  recording: null, // { meetingId, startedAt, offset }
  timer: null,
  streams: new Map(), // requestId -> { target: "ai" | HTMLElement, meetingId, text }
  enhancing: null, // { id, meetingId } while AI notes are being written
  chat: { scope: undefined, messages: [], busy: false },
  patch: {},
  saveTimer: null,
  saving: null,
};

// ---------- helpers ----------

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

const rid = () => Math.random().toString(36).slice(2) + Date.now().toString(36);

function fmtDuration(secs) {
  secs = Math.max(0, Math.round(secs || 0));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function fmtTs(secs) {
  const s = Math.floor(secs || 0);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

function dayLabel(d) {
  const today = new Date();
  const startOf = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((startOf(today) - startOf(d)) / 86400000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  if (diff < 7) return d.toLocaleDateString(undefined, { weekday: "long" });
  return d.toLocaleDateString(undefined, { day: "numeric", month: "long", year: d.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}

function toast(message, opts = {}) {
  const el = document.createElement("div");
  el.className = "toast" + (opts.error ? " error" : "");
  el.textContent = message;
  if (opts.action) {
    const b = document.createElement("button");
    b.className = "btn btn-small";
    b.style.marginLeft = "10px";
    b.textContent = opts.action.label;
    b.onclick = () => {
      opts.action.run();
      el.remove();
    };
    el.append(b);
  }
  $("#toasts").append(el);
  setTimeout(() => el.remove(), opts.ms ?? (opts.error ? 8000 : 3500));
}

const errMsg = (e) => (typeof e === "string" ? e : e?.message || JSON.stringify(e));

function confirmDialog(message, okLabel = "OK") {
  const dlg = $("#confirm");
  $("#confirm-msg").textContent = message;
  $("#confirm-ok").textContent = okLabel;
  dlg.returnValue = "";
  dlg.showModal();
  return new Promise((resolve) => dlg.addEventListener("close", () => resolve(dlg.returnValue === "ok"), { once: true }));
}

function autoGrow(el) {
  el.style.height = "auto";
  el.style.height = el.scrollHeight + 4 + "px";
}

const md = (text) => invoke("render_markdown", { md: text || "" });

// ---------- settings ----------

async function loadSettings() {
  state.settings = await invoke("get_settings");
  $("#key-banner").hidden = state.settings.has_api_key;
  $("#empty-folder").textContent = state.settings.settings.notes_dir;
}

async function loadTemplates() {
  state.templates = await invoke("list_templates");
  const opts = state.templates.map((t) => `<option value="${esc(t.id)}">${esc(t.name)}</option>`).join("");
  $("#template").innerHTML = opts;
  $("#s-template").innerHTML = opts;
  syncTemplateSelect();
}

function syncTemplateSelect() {
  const def = state.settings?.settings.default_template || "general";
  const want = state.current?.template || def;
  const has = state.templates.some((t) => t.id === want);
  $("#template").value = has ? want : state.templates[0]?.id || "";
}

async function openSettings() {
  const s = state.settings.settings;
  $("#api-key").value = "";
  renderKeyStatus();
  $("#s-chat-model").value = s.chat_model;
  $("#s-tr-model").value = s.transcription_model;
  $("#s-base-url").value = s.base_url;
  $("#s-language").value = s.language;
  $("#s-chunk").value = String(s.chunk_seconds);
  if (!$("#s-chunk").value) {
    $("#s-chunk").insertAdjacentHTML("beforeend", `<option value="${s.chunk_seconds}">${s.chunk_seconds} s</option>`);
    $("#s-chunk").value = String(s.chunk_seconds);
  }
  $("#s-system").checked = s.capture_system_audio;
  $("#system-help").textContent = state.settings.system_audio_supported
    ? "Uses Windows loopback capture — no virtual audio driver needed. Headphones give the cleanest transcript."
    : `System-audio capture is Windows-only; on ${state.settings.platform} only your microphone is recorded.`;
  $("#s-me").value = s.my_name;
  $("#s-them").value = s.their_name;
  $("#s-dir").value = s.notes_dir;
  $("#s-custom").value = s.custom_instructions;
  $("#s-template").value = s.default_template;
  $("#test-result").textContent = "";
  $("#settings-error").textContent = "";

  const mics = await invoke("list_input_devices").catch(() => []);
  $("#s-mic").innerHTML =
    `<option value="">System default</option>` + mics.map((m) => `<option value="${esc(m)}">${esc(m)}</option>`).join("");
  if (s.mic_device && !mics.includes(s.mic_device)) {
    $("#s-mic").insertAdjacentHTML("beforeend", `<option value="${esc(s.mic_device)}">${esc(s.mic_device)} (not connected)</option>`);
  }
  $("#s-mic").value = s.mic_device;
  $("#settings").showModal();
}

function renderKeyStatus() {
  const v = state.settings;
  const el = $("#key-status");
  el.className = "help" + (v.has_api_key ? " ok" : "");
  el.textContent = v.has_api_key
    ? `Key saved (${v.api_key_hint}) in your system credential manager.`
    : "No key yet. Get one at platform.openai.com → API keys. It stays on this computer.";
}

async function saveKey(remove = false) {
  const key = remove ? "" : $("#api-key").value.trim();
  if (!remove && !key) return $("#api-key").focus();
  try {
    state.settings = await invoke("set_api_key", { key });
    $("#api-key").value = "";
    renderKeyStatus();
    $("#key-banner").hidden = state.settings.has_api_key;
    toast(remove ? "API key removed" : "API key saved");
  } catch (e) {
    $("#key-status").className = "help bad";
    $("#key-status").textContent = errMsg(e);
  }
}

async function saveSettings(ev) {
  ev.preventDefault();
  const pendingKey = $("#api-key").value.trim();
  if (pendingKey) await saveKey();
  const s = {
    ...state.settings.settings,
    chat_model: $("#s-chat-model").value.trim(),
    transcription_model: $("#s-tr-model").value.trim(),
    base_url: $("#s-base-url").value.trim(),
    language: $("#s-language").value.trim(),
    chunk_seconds: parseInt($("#s-chunk").value, 10) || 20,
    capture_system_audio: $("#s-system").checked,
    mic_device: $("#s-mic").value,
    my_name: $("#s-me").value.trim(),
    their_name: $("#s-them").value.trim(),
    notes_dir: $("#s-dir").value.trim(),
    custom_instructions: $("#s-custom").value,
    default_template: $("#s-template").value,
  };
  const dirChanged = s.notes_dir !== state.settings.settings.notes_dir;
  try {
    await flushSave();
    state.settings = await invoke("save_settings", { settings: s });
    $("#settings").close();
    $("#empty-folder").textContent = state.settings.settings.notes_dir;
    if (dirChanged) {
      state.current = null;
      showEditor(false);
    }
    await Promise.all([loadTemplates(), refreshList()]);
    toast("Settings saved");
  } catch (e) {
    $("#settings-error").textContent = errMsg(e);
  }
}

async function testConnection() {
  const out = $("#test-result");
  if ($("#api-key").value.trim()) await saveKey();
  // Test against the values currently in the form.
  try {
    state.settings = await invoke("save_settings", {
      settings: {
        ...state.settings.settings,
        chat_model: $("#s-chat-model").value.trim(),
        transcription_model: $("#s-tr-model").value.trim(),
        base_url: $("#s-base-url").value.trim(),
      },
    });
  } catch (e) {
    out.className = "help bad";
    out.textContent = errMsg(e);
    return;
  }
  out.className = "help";
  out.textContent = "Testing…";
  try {
    out.textContent = await invoke("test_connection");
    out.className = "help ok";
  } catch (e) {
    out.className = "help bad";
    out.textContent = errMsg(e);
  }
}

// ---------- meeting list ----------

async function refreshList() {
  const query = $("#search").value;
  state.meetings = await invoke("list_meetings", { query });
  renderList();
}

function renderList() {
  const list = $("#meeting-list");
  if (!state.meetings.length) {
    list.innerHTML = `<div class="list-empty">${$("#search").value ? "No matching notes." : "No meetings yet."}</div>`;
    return;
  }
  let html = "";
  let lastGroup = "";
  for (const m of state.meetings) {
    const d = new Date(m.created);
    const group = isNaN(d) ? "Earlier" : dayLabel(d);
    if (group !== lastGroup) {
      html += `<div class="group-label">${esc(group)}</div>`;
      lastGroup = group;
    }
    const time = isNaN(d) ? "" : d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
    const live = state.recording?.meetingId === m.id ? `<span class="live" title="Recording"></span>` : "";
    const sub = [time, m.duration_secs ? fmtDuration(m.duration_secs) : "", m.snippet].filter(Boolean).join(" · ");
    html += `<button class="item${state.current?.id === m.id ? " active" : ""}" data-id="${esc(m.id)}">
      <div class="item-title">${live}<span class="t">${esc(m.title || "Untitled meeting")}</span></div>
      <div class="item-sub">${esc(sub)}</div></button>`;
  }
  list.innerHTML = html;
}

// ---------- editor ----------

function showEditor(show) {
  $("#editor").hidden = !show;
  $("#empty").hidden = show;
  $("#dock").hidden = !show;
}

async function openMeeting(id) {
  if (state.current?.id === id) return;
  await flushSave();
  try {
    state.current = await invoke("get_meeting", { id });
  } catch (e) {
    toast(errMsg(e), { error: true });
    return refreshList();
  }
  state.aiEditing = false;
  closeChat(true);
  renderEditor();
  renderList();
}

function renderEditor() {
  const m = state.current;
  showEditor(!!m);
  if (!m) return;
  $("#title").value = m.title;
  $("#attendees").value = (m.attendees || []).join(", ");
  const d = new Date(m.created);
  $("#meta-date").textContent = isNaN(d)
    ? ""
    : d.toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" });
  $("#my-notes").value = m.my_notes;
  $("#save-state").textContent = "";
  syncTemplateSelect();
  renderMeta();
  renderAi();
  renderTranscript();
  setTab(m.enhanced_notes || state.enhancing?.meetingId === m.id ? "ai" : "mine");
  requestAnimationFrame(() => autoGrow($("#my-notes")));
}

function renderMeta() {
  const m = state.current;
  const dur = $("#meta-duration");
  dur.hidden = !m.duration_secs;
  dur.textContent = fmtDuration(m.duration_secs);
  $("#transcript-count").textContent = m.transcript.length ? m.transcript.length : "";
  $("#ai-dot").hidden = !m.enhanced_notes;
}

function setTab(tab) {
  state.tab = tab;
  $$(".tab[data-tab]").forEach((t) => t.classList.toggle("active", t.dataset.tab === tab));
  $(".pane-mine").hidden = tab !== "mine";
  $(".pane-ai").hidden = tab !== "ai";
  if (tab === "mine") requestAnimationFrame(() => autoGrow($("#my-notes")));
}

async function renderAi() {
  const m = state.current;
  if (!m) return;
  const streaming = state.enhancing?.meetingId === m.id;
  const text = streaming ? state.streams.get(state.enhancing.id)?.text ?? "" : m.enhanced_notes;
  const hasText = !!text?.trim();
  const tpl = state.templates.find((t) => t.id === m.template);
  $("#ai-template-label").textContent = streaming ? "Writing notes…" : hasText && tpl ? `Template: ${tpl.name}` : "";
  $("#ai-empty").hidden = hasText || streaming;
  $("#ai-edit").hidden = !hasText || streaming;
  $("#ai-copy").hidden = !hasText || streaming;
  $("#ai-edit").textContent = state.aiEditing ? "Done" : "Edit";
  $("#ai-edit-input").hidden = !state.aiEditing;
  $("#ai-view").hidden = state.aiEditing;
  if (state.aiEditing) {
    $("#ai-edit-input").value = m.enhanced_notes;
    requestAnimationFrame(() => autoGrow($("#ai-edit-input")));
  } else {
    const html = await md(text);
    if (state.current?.id !== m.id) return;
    $("#ai-view").innerHTML = html;
    $("#ai-view").classList.toggle("streaming", streaming);
  }
}

function renderTranscript() {
  const m = state.current;
  const list = $("#transcript-list");
  if (!m.transcript.length) {
    const recordingHere = state.recording?.meetingId === m.id;
    list.innerHTML = `<div class="transcript-empty">${
      recordingHere ? "Listening… text appears here every few seconds." : "No transcript yet. Press Record to capture this meeting."
    }</div>`;
    return;
  }
  const myName = state.settings?.settings.my_name || "Me";
  list.innerHTML = m.transcript
    .map(
      (s) => `<div class="seg${s.speaker === myName ? " me" : ""}">
        <div class="who">${esc(s.speaker)} · ${fmtTs(s.start)}</div>
        <div class="bubble">${esc(s.text)}</div></div>`
    )
    .join("");
}

function toggleTranscript(force) {
  const el = $("#transcript");
  const show = force ?? el.hidden;
  el.hidden = !show;
  $("#toggle-transcript").classList.toggle("active", show);
  if (show) {
    const list = $("#transcript-list");
    list.scrollTop = list.scrollHeight;
  }
}

// ---------- autosave ----------

function queueSave(field, value) {
  if (!state.current) return;
  state.patch[field] = value;
  state.current[field] = value;
  $("#save-state").textContent = "Editing…";
  clearTimeout(state.saveTimer);
  state.saveTimer = setTimeout(flushSave, 700);
}

async function flushSave() {
  clearTimeout(state.saveTimer);
  if (state.saving) await state.saving;
  const m = state.current;
  if (!m || !Object.keys(state.patch).length) return;
  const patch = state.patch;
  state.patch = {};
  const id = m.id;
  state.saving = (async () => {
    try {
      const saved = await invoke("update_meeting", { id, patch });
      if (state.current?.id === id) {
        state.current.path = saved.path;
        state.current.transcript = saved.transcript;
        $("#save-state").textContent = "Saved";
      }
      const item = state.meetings.find((x) => x.id === id);
      if (item && "title" in patch) {
        item.title = saved.title;
        renderList();
      }
    } catch (e) {
      Object.assign(state.patch, patch, state.patch);
      $("#save-state").textContent = "Not saved";
      toast(`Couldn't save: ${errMsg(e)}`, { error: true });
    }
  })();
  await state.saving;
  state.saving = null;
}

// ---------- create / delete ----------

async function newMeeting(record = false) {
  await flushSave();
  try {
    const m = await invoke("create_meeting", { title: "" });
    state.current = m;
    state.tab = "mine";
    state.aiEditing = false;
    closeChat(true);
    $("#search").value = "";
    await refreshList();
    renderEditor();
    if (record) {
      await startRecording();
      $("#my-notes").focus();
    } else {
      $("#title").focus();
    }
  } catch (e) {
    toast(errMsg(e), { error: true });
  }
}

async function deleteCurrent() {
  const m = state.current;
  if (!m) return;
  const ok = await confirmDialog(`Move “${m.title || "Untitled meeting"}” to the trash folder? You can restore it from _trash in your notes folder.`, "Move to trash");
  if (!ok) return;
  try {
    state.patch = {};
    await invoke("delete_meeting", { id: m.id });
    state.current = null;
    showEditor(false);
    await refreshList();
    toast("Moved to trash");
  } catch (e) {
    toast(errMsg(e), { error: true });
  }
}

// ---------- recording ----------

function renderRecording() {
  const rec = state.recording;
  const here = rec && state.current && rec.meetingId === state.current.id;
  const btn = $("#rec-btn");
  btn.classList.toggle("recording", !!here);
  $("#rec-info").hidden = !here;
  if (rec && !here) {
    $("#rec-label").textContent = "Recording elsewhere";
    btn.title = "Another meeting is recording — open it from the sidebar to stop.";
  } else {
    $("#rec-label").textContent = here ? "Stop" : state.current?.transcript?.length ? "Resume" : "Record";
    btn.title = "";
  }
  btn.disabled = !!rec && !here;
}

function tickTimer() {
  const rec = state.recording;
  if (!rec) return;
  $("#rec-time").textContent = fmtDuration(rec.offset + (Date.now() - rec.startedAt) / 1000);
}

async function startRecording() {
  if (!state.current) return;
  if (!state.settings.has_api_key) {
    toast("Add your OpenAI API key first — transcription runs on OpenAI.", { error: true });
    return openSettings();
  }
  await flushSave();
  const btn = $("#rec-btn");
  btn.disabled = true;
  $("#rec-label").textContent = "Starting…";
  try {
    const status = await invoke("start_recording", { meetingId: state.current.id });
    beginRecordingUi(status);
    for (const w of status.warnings) toast(w, { ms: 9000 });
    toggleTranscript(true);
  } catch (e) {
    toast(errMsg(e), { error: true });
  }
  btn.disabled = false;
  renderRecording();
}

function beginRecordingUi(status) {
  state.recording = {
    meetingId: status.meeting_id,
    startedAt: Date.now() - status.elapsed_secs * 1000,
    offset: status.offset_secs,
  };
  clearInterval(state.timer);
  state.timer = setInterval(tickTimer, 500);
  tickTimer();
  renderRecording();
  renderList();
  if (state.current?.id === status.meeting_id) renderTranscript();
}

async function stopRecording() {
  const btn = $("#rec-btn");
  btn.disabled = true;
  $("#rec-label").textContent = "Finishing…";
  try {
    const m = await invoke("stop_recording");
    clearInterval(state.timer);
    const rec = state.recording;
    state.recording = null;
    setLevels(0, 0);
    if (m && state.current?.id === m.id) {
      state.current.transcript = m.transcript;
      state.current.duration_secs = m.duration_secs;
      renderMeta();
      renderTranscript();
    }
    await refreshList();
    // Like Granola: write up the notes as soon as the meeting ends.
    if (m && rec && state.current?.id === m.id && m.transcript.length && !state.current.enhanced_notes.trim()) {
      enhance();
    } else if (m && !m.transcript.length) {
      toast("No speech was captured. Check your microphone in Settings.", { ms: 6000 });
    }
  } catch (e) {
    toast(errMsg(e), { error: true });
  }
  btn.disabled = false;
  renderRecording();
}

function setLevels(me, them) {
  const w = (l) => Math.min(100, Math.sqrt(l) * 170) + "%";
  $("#lvl-me").style.width = w(me);
  $("#lvl-them").style.width = w(them);
}

// ---------- AI notes ----------

async function enhance() {
  const m = state.current;
  if (!m || state.enhancing) return;
  if (!state.settings.has_api_key) {
    toast("Add your OpenAI API key in Settings first.", { error: true });
    return openSettings();
  }
  await flushSave();
  const previous = m.enhanced_notes;
  const templateId = $("#template").value;
  const id = rid();
  state.streams.set(id, { target: "ai", text: "" });
  state.enhancing = { id, meetingId: m.id };
  state.aiEditing = false;
  setTab("ai");
  renderAi();
  const btn = $("#enhance-btn");
  btn.disabled = true;
  try {
    const saved = await invoke("enhance_notes", { meetingId: m.id, templateId, requestId: id });
    if (state.current?.id === saved.id) {
      state.current.enhanced_notes = saved.enhanced_notes;
      state.current.template = saved.template;
      if (!$("#title").value.trim() && saved.title) {
        state.current.title = saved.title;
        $("#title").value = saved.title;
      }
    }
    if (previous.trim()) {
      toast("Notes regenerated", {
        action: {
          label: "Undo",
          run: async () => {
            const back = await invoke("update_meeting", { id: saved.id, patch: { enhanced_notes: previous } });
            if (state.current?.id === back.id) {
              state.current.enhanced_notes = back.enhanced_notes;
              renderAi();
              renderMeta();
            }
          },
        },
        ms: 8000,
      });
    }
    refreshList();
  } catch (e) {
    toast(errMsg(e), { error: true });
  } finally {
    state.streams.delete(id);
    state.enhancing = null;
    btn.disabled = false;
    if (state.current) {
      renderAi();
      renderMeta();
      if (!state.current.enhanced_notes) setTab("mine");
    }
  }
}

function onDelta({ request_id, delta }) {
  const s = state.streams.get(request_id);
  if (!s) return;
  s.text += delta;
  if (s.queued) return;
  s.queued = true;
  // Re-render at most ~10x/second rather than on every token.
  setTimeout(async () => {
    s.queued = false;
    if (!state.streams.has(request_id)) return;
    if (s.target === "ai") {
      if (state.enhancing?.meetingId === state.current?.id && !state.aiEditing) renderAi();
    } else {
      const html = await md(s.text);
      if (state.streams.has(request_id)) {
        s.target.innerHTML = html;
        scrollChat();
      }
    }
  }, 90);
}

// ---------- chat ----------

function openChat(scope) {
  if (state.chat.scope !== scope) {
    state.chat = { scope, messages: [], busy: false };
    $("#chat-log").innerHTML = "";
  }
  $("#chat-title").textContent = scope ? "Ask about this meeting" : "Ask across all meetings";
  $("#chat-input").placeholder = scope ? "Ask a follow-up…" : "e.g. What did we decide about pricing last week?";
  $("#chat").hidden = false;
}

function closeChat(reset = false) {
  $("#chat").hidden = true;
  if (reset) {
    state.chat = { scope: undefined, messages: [], busy: false };
    $("#chat-log").innerHTML = "";
  }
}

function scrollChat() {
  const log = $("#chat-log");
  log.scrollTop = log.scrollHeight;
}

async function ask(question, scope) {
  question = question.trim();
  if (!question || state.chat.busy) return;
  if (!state.settings.has_api_key) {
    toast("Add your OpenAI API key in Settings first.", { error: true });
    return openSettings();
  }
  await flushSave();
  openChat(scope);
  const chat = state.chat;
  chat.busy = true;
  chat.messages.push({ role: "user", content: question });
  const log = $("#chat-log");
  log.insertAdjacentHTML("beforeend", `<div class="msg user">${esc(question)}</div>`);
  const out = document.createElement("div");
  out.className = "msg assistant prose streaming";
  log.append(out);
  scrollChat();
  const id = rid();
  state.streams.set(id, { target: out, text: "" });
  try {
    const answer = await invoke("ask", { meetingId: scope ?? null, messages: chat.messages, requestId: id });
    chat.messages.push({ role: "assistant", content: answer });
    out.innerHTML = await md(answer);
  } catch (e) {
    chat.messages.pop();
    out.className = "msg error";
    out.textContent = errMsg(e);
  } finally {
    out.classList.remove("streaming");
    state.streams.delete(id);
    chat.busy = false;
    scrollChat();
  }
}

// ---------- wiring ----------

function wire() {
  $("#new-meeting").onclick = () => newMeeting(false);
  $("#empty-start").onclick = () => newMeeting(true);
  $("#empty-folder").onclick = () => invoke("open_path", { which: "notes" }).catch((e) => toast(errMsg(e), { error: true }));
  $("#open-folder").onclick = $("#empty-folder").onclick;
  $("#open-settings").onclick = openSettings;
  $$("[data-open-settings]").forEach((b) => (b.onclick = openSettings));

  let searchTimer;
  $("#search").addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(refreshList, 200);
  });
  $("#meeting-list").addEventListener("click", (e) => {
    const item = e.target.closest(".item");
    if (item) openMeeting(item.dataset.id);
  });

  $("#title").addEventListener("input", (e) => queueSave("title", e.target.value));
  $("#title").addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      $("#my-notes").focus();
    }
  });
  $("#attendees").addEventListener("input", (e) =>
    queueSave("attendees", e.target.value.split(",").map((s) => s.trim()).filter(Boolean))
  );
  $("#my-notes").addEventListener("input", (e) => {
    queueSave("my_notes", e.target.value);
    autoGrow(e.target);
  });
  $("#ai-edit-input").addEventListener("input", (e) => {
    queueSave("enhanced_notes", e.target.value);
    autoGrow(e.target);
  });
  $("#my-notes").addEventListener("keydown", (e) => {
    // Tab indents list items instead of leaving the editor.
    if (e.key !== "Tab") return;
    e.preventDefault();
    const ta = e.target;
    const { selectionStart: s, value } = ta;
    const lineStart = value.lastIndexOf("\n", s - 1) + 1;
    if (e.shiftKey) {
      if (value.startsWith("  ", lineStart)) {
        ta.setRangeText("", lineStart, lineStart + 2, "preserve");
        ta.selectionStart = ta.selectionEnd = Math.max(lineStart, s - 2);
      }
    } else {
      ta.setRangeText("  ", lineStart, lineStart, "preserve");
      ta.selectionStart = ta.selectionEnd = s + 2;
    }
    ta.dispatchEvent(new Event("input"));
  });

  $$(".tab[data-tab]").forEach((t) => (t.onclick = () => setTab(t.dataset.tab)));
  $("#toggle-transcript").onclick = () => toggleTranscript();
  $("#close-transcript").onclick = () => toggleTranscript(false);

  $("#ai-edit").onclick = async () => {
    if (state.aiEditing) await flushSave();
    state.aiEditing = !state.aiEditing;
    renderAi();
    if (state.aiEditing) $("#ai-edit-input").focus();
  };
  $("#ai-copy").onclick = () => navigator.clipboard.writeText(state.current.enhanced_notes).then(() => toast("Copied"));

  $("#rec-btn").onclick = () => (state.recording ? stopRecording() : startRecording());
  $("#enhance-btn").onclick = enhance;

  $("#ask-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const q = $("#ask-input").value;
    $("#ask-input").value = "";
    ask(q, state.current?.id);
  });
  $("#chat-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const q = $("#chat-input").value;
    $("#chat-input").value = "";
    ask(q, state.chat.scope);
  });
  $("#ask-all").onclick = () => {
    openChat(null);
    $("#chat-input").focus();
  };
  $("#chat-close").onclick = () => closeChat();
  $("#chat-clear").onclick = () => {
    const scope = state.chat.scope;
    closeChat(true);
    openChat(scope);
  };

  $("#doc-menu-btn").onclick = (e) => {
    e.stopPropagation();
    $("#doc-menu").hidden = !$("#doc-menu").hidden;
  };
  document.addEventListener("click", () => ($("#doc-menu").hidden = true));
  $("#doc-menu").addEventListener("click", async (e) => {
    const action = e.target.dataset.action;
    if (!action || !state.current) return;
    await flushSave();
    if (action === "reveal") invoke("open_path", { which: "meeting", id: state.current.id }).catch((er) => toast(errMsg(er), { error: true }));
    if (action === "copy") {
      const m = state.current;
      const text = `# ${m.title || "Untitled meeting"}\n\n${m.enhanced_notes || m.my_notes}`;
      navigator.clipboard.writeText(text).then(() => toast("Copied"));
    }
    if (action === "delete") deleteCurrent();
  });

  // Settings dialog
  $("#settings-form").addEventListener("submit", (e) => {
    if (e.submitter?.value === "cancel") return;
    saveSettings(e);
  });
  $("#save-key").onclick = () => saveKey(false);
  $("#remove-key").onclick = () => saveKey(true);
  $("#test-conn").onclick = testConnection;
  $("#s-open-dir").onclick = () => invoke("open_path", { which: "notes" }).catch((e) => toast(errMsg(e), { error: true }));
  $("#s-open-templates").onclick = () =>
    invoke("open_path", { which: "templates" })
      .then(() => toast("Edit or add .md files there; changes appear next time you open Settings.", { ms: 6000 }))
      .catch((e) => toast(errMsg(e), { error: true }));
  $("#settings").addEventListener("close", () => loadTemplates().catch(() => {}));

  document.addEventListener("keydown", (e) => {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === "n") {
      e.preventDefault();
      newMeeting(false);
    } else if (mod && e.key.toLowerCase() === "k") {
      e.preventDefault();
      $("#search").focus();
      $("#search").select();
    } else if (mod && e.key === "Enter" && state.current) {
      e.preventDefault();
      enhance();
    } else if (mod && e.key.toLowerCase() === "s") {
      e.preventDefault();
      flushSave();
    } else if (e.key === "Escape" && !$("#chat").hidden && !document.querySelector("dialog[open]")) {
      closeChat();
    }
  });
  window.addEventListener("blur", flushSave);

  // Links in rendered notes must never navigate the app window away.
  document.addEventListener("click", (e) => {
    const a = e.target.closest(".prose a[href]");
    if (!a) return;
    e.preventDefault();
    navigator.clipboard.writeText(a.getAttribute("href")).then(() => toast("Link copied to clipboard"));
  });
}

async function wireEvents() {
  await listen("audio-level", (e) => setLevels(e.payload.me, e.payload.them));
  await listen("transcript-segment", (e) => {
    const { meeting_id, segment } = e.payload;
    const m = state.current;
    if (!m || m.id !== meeting_id) return;
    const list = $("#transcript-list");
    const atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 60;
    const idx = m.transcript.findIndex((s) => s.start > segment.start);
    if (idx === -1) m.transcript.push(segment);
    else m.transcript.splice(idx, 0, segment);
    renderTranscript();
    renderMeta();
    if (atBottom) list.scrollTop = list.scrollHeight;
  });
  await listen("transcription-pending", (e) => {
    $("#pending").textContent = e.payload > 0 ? `transcribing ${e.payload}…` : "";
  });
  await listen("recording-error", (e) => toast(e.payload.message, { error: true }));
  await listen("ai-delta", (e) => onDelta(e.payload));
  await listen("app-closing", () => toast("Finishing the transcript before closing…", { ms: 30000 }));
}

async function init() {
  wire();
  await wireEvents();
  await loadSettings();
  await loadTemplates();
  await refreshList();
  const status = await invoke("recording_status");
  if (status) {
    await openMeeting(status.meeting_id);
    beginRecordingUi(status);
  }
  showEditor(!!state.current);
  renderRecording();
  if (!state.settings.has_api_key) setTimeout(openSettings, 300);
}

init().catch((e) => toast(`Startup failed: ${errMsg(e)}`, { error: true }));
