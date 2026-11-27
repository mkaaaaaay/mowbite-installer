// The page of the installer: shows the state the installer serves under its random path and sends the owner's answers.
"use strict";

const LANG = (navigator.language || "en").toLowerCase().startsWith("de") ? "de" : "en";
const PREVIEW = location.protocol === "file:" || new URLSearchParams(location.search).has("preview");

const T = {
  start_title: ["Was soll der Installer tun?", "What should the installer do?"],
  goal_install: ["MowBite OS installieren", "Install MowBite OS"],
  goal_install_text: ["Sichert den ganzen Speicher der Tron, prüft sie und schreibt dann MowBite OS.",
    "Backs up the whole flash of the Tron, checks it and then writes MowBite OS."],
  goal_check: ["Nur sichern und prüfen", "Back up and check only"],
  goal_check_text: ["Schreibt nichts auf die Tron. Installieren geht später mit derselben Sicherung weiter.",
    "Writes nothing to the Tron. Installing later goes on from the same backup."],
  goal_back: ["Hersteller-System zurückschreiben", "Write the vendor system back"],
  goal_back_text: ["Schreibt die Sicherung aus dem Ordner zurück auf die Tron.",
    "Writes the backup in the folder back to the Tron."],
  go_install: ["Sichern und installieren", "Back up and install"],
  go_check: ["Sichern und prüfen", "Back up and check"],
  go_back: ["Sicherung zurückschreiben", "Write the backup back"],
  go_choose: ["Erst ein Ziel wählen", "Choose a goal first"],
  store: ["Sicherungsordner", "Backup folder"],
  store_hint: ["Hier liegen die Sicherung mit den Schlüsseln und dem WLAN der Tron: nicht weitergeben.",
    "The backup with the keys and Wi-Fi of the Tron goes here: keep it to yourself."],
  wifi_name: ["WLAN-Name", "Wi-Fi name"],
  wifi_key: ["WLAN-Schlüssel", "Wi-Fi key"],
  wifi_key_hint: ["Kommt nur auf die Tron, nie ins Protokoll oder in den Sicherungsordner.",
    "Goes to the Tron only, never into the log or the backup folder."],
  wifi_needed: ["Beide Felder braucht der erste Start: die Tron meldet sich darüber, damit der Installer sie findet.",
    "The first start needs both: the Tron reports over it so that the installer finds it."],
  show: ["Zeigen", "Show"],
  hide: ["Verbergen", "Hide"],
  run_install: ["MowBite OS wird installiert", "Installing MowBite OS"],
  run_check: ["Sichern und prüfen", "Back up and check"],
  run_back: ["Hersteller-System zurück", "Vendor system back"],
  end_ok_install: ["MowBite OS ist installiert", "MowBite OS is installed"],
  end_ok_check: ["Gesichert und geprüft", "Backed up and checked"],
  end_ok_back: ["Hersteller-System ist zurück", "Vendor system is back"],
  end_stopped: ["Abgebrochen", "Stopped"],
  end_keep: ["Den Sicherungsordner aufbewahren, er ist der Weg zurück, und nicht weitergeben.",
    "Keep the backup folder, it is the way back, and do not share it."],
  open_app: ["MowBite öffnen", "Open MowBite"],
  waiting: ["Wartet auf deine Antwort", "Waits for your answer"],
  of: ["{done} von {total} GB", "{done} of {total} GB"],
  step_connect: ["Verbinden", "Connect"],
  step_backup: ["Sichern", "Back up"],
  step_check: ["Prüfen", "Check"],
  step_agree: ["Bestätigen", "Confirm"],
  step_prepare: ["Vorbereiten", "Prepare"],
  step_write: ["Schreiben", "Write"],
  step_first_start: ["Erster Start", "First start"],
  step_done: ["Fertig", "Done"],
  guide_title: ["Tron im USB-Modus anschließen", "Connect the Tron in USB mode"],
  setup_wifi_title: ["WLAN der Tron", "Wi-Fi of the Tron"],
  setup_wifi_text: ["Findet die Tron dein WLAN nicht, öffnet sie ihr eigenes. Mit diesem PC oder dem Handy verbinden, der Installer findet sie dann.",
    "Should the Tron not find your Wi-Fi, it opens its own. Join it with this PC or your phone, the installer finds it then."],
  log: ["Protokoll", "Log"],
  copy: ["Kopieren", "Copy"],
  copied: ["Kopiert", "Copied"],
  end_close: ["Du kannst dieses Fenster schließen. Der Installer endet von selbst.",
    "You can close this window. The installer ends by itself."],
  lost: ["Keine Verbindung zum Installer. Läuft er noch? Im Terminal steht, was passiert ist.",
    "No connection to the installer. Is it still running? The terminal shows what happened."],
};

const $ = (id) => document.getElementById(id);
const t = (key, values) => {
  let text = (T[key] || [key, key])[LANG === "de" ? 0 : 1];
  for (const [name, value] of Object.entries(values || {})) text = text.replace(`{${name}}`, value);
  return text;
};
// Texts the installer sends come as {de, en}.
const pick = (text) => (text && typeof text === "object" ? text[LANG] || text.en : text || "");

const CHECK = '<svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>';
const CROSS = '<svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17"/></svg>';
const BANG = '<svg viewBox="0 0 24 24"><path d="M12 6v8M12 18v.5"/></svg>';
const DASH = '<svg viewBox="0 0 24 24"><path d="M7 12h10"/></svg>';
const MARKS = { ok: CHECK, failed: CROSS, warn: BANG, declined: DASH };
const gb = (bytes) => (bytes / 1e9).toLocaleString(LANG, { minimumFractionDigits: 1, maximumFractionDigits: 1 });

let state = null;
let goal = null;
let logFrom = 0;
let sending = false;

function el(tag, attrs, children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (key === "text") node.textContent = value;
    else if (key === "html") node.innerHTML = value;
    else node.setAttribute(key, value);
  }
  for (const child of children || []) if (child) node.append(child);
  return node;
}

function withCode(text, code) {
  const span = el("span", { text });
  return code ? [span, el("span", { class: "code", text: code })] : [span];
}

// The parts of the page, each shown from the state alone.

function renderStart(s) {
  $("start").hidden = s.view !== "start";
  if (s.view !== "start") return;
  $("before").textContent = pick(s.before);
  $("before").hidden = !s.before;
  const goals = $("goals");
  if (!goals.childElementCount) {
    for (const id of ["install", "check", "back"]) {
      const input = el("input", { type: "radio", name: "goal", value: id });
      input.addEventListener("change", () => {
        goal = id;
        render(state, false);
      });
      goals.append(el("label", { class: "choice" }, [input,
        el("span", {}, [el("div", { class: "title", text: t("goal_" + id) }),
          el("div", { class: "secondary", text: t(`goal_${id}_text`) })])]));
    }
  }
  if (!$("store").value && s.store) $("store").value = s.store;
  $("store_hint").textContent = t("store_hint");
  $("wifi_form").hidden = goal !== "install";
  $("start_error").hidden = !s.start_error;
  $("start_error").replaceChildren(...withCode(pick(s.start_error?.text), s.start_error?.code));
  $("go").textContent = goal ? t("go_" + goal) : t("go_choose");
  $("go").className = goal ? "button primary" : "button";
  $("go").disabled = !goal || sending;
}

function renderRun(s) {
  $("run").hidden = s.view !== "run";
  if (s.view !== "run") return;
  $("run_title").textContent = !s.end ? t("run_" + s.goal) : s.end.tone === "good" ? t("end_ok_" + s.goal) : t("end_stopped");
  $("steps").replaceChildren(...s.steps.map((step) => {
    const text = el("div", {}, [el("div", { class: "title", text: t("step_" + step.id) })]);
    if (step.state === "waiting") text.append(el("div", { class: "secondary", text: t("waiting") }));
    if (step.state === "running") {
      if (s.detail) text.append(el("div", { class: "secondary", text: pick(s.detail) }));
      if (s.progress) {
        const share = Math.min(100, (100 * s.progress.done) / Math.max(1, s.progress.total));
        const fill = el("span");
        fill.style.width = `${share}%`;
        text.append(el("div", { class: "bar", role: "progressbar", "aria-valuenow": Math.floor(share),
          "aria-valuemin": "0", "aria-valuemax": "100" }, [fill]));
        if (s.progress.total > 0) {
          text.append(el("div", { class: "bar_text", text: t("of", { done: gb(s.progress.done), total: gb(s.progress.total) }) }));
        }
      }
    }
    return el("li", { "data-state": step.state }, [el("span", { class: "mark", html: MARKS[step.state] || "" }), text]);
  }));
  $("guide_card").hidden = !s.guide;
  if (s.guide) $("guide").replaceChildren(...s.guide.map((line) => el("li", { text: pick(line) })));
}

function renderQuestion(s) {
  const q = s.question;
  $("question").hidden = !q;
  if (!q) return;
  const box = $("question_text");
  box.dataset.tone = q.tone || "warn";
  box.setAttribute("role", q.tone === "bad" ? "alert" : "status");
  const parts = [el("p", {}, withCode(pick(q.text), q.code))];
  if (q.findings?.length) {
    // "Warning: text": the kind in bold.
    parts.push(el("ul", {}, q.findings.map((f) => {
      const [kind, rest] = pick(f.text).split(/: (.*)/s);
      const line = rest === undefined ? [el("span", { text: kind })] : [el("strong", { text: kind + ": " }), el("span", { text: rest })];
      return el("li", {}, f.details ? [...line, el("span", { class: "code", text: f.details })] : line);
    })));
  }
  box.replaceChildren(...parts);
  // Two buttons of the same weight, no focus, no Enter: each names what it does.
  $("answers").className = q.answers.length === 1 ? "buttons one" : "buttons";
  $("answers").replaceChildren(...q.answers.map((answer) => {
    const button = el("button", { class: "button", type: "button", text: pick(answer.label) });
    button.disabled = sending;
    button.addEventListener("click", () => send("answer", { question: q.id, answer: answer.value }));
    return answer.hint ? el("div", { class: "answer" }, [button, el("div", { class: "secondary", text: pick(answer.hint) })]) : button;
  }));
}

function renderSetupWifi(s) {
  $("setup_wifi").hidden = !s.setup_wifi;
  if (!s.setup_wifi) return;
  $("setup_name").textContent = s.setup_wifi.name;
  $("setup_qr").innerHTML = s.setup_wifi.qr;
  $("setup_qr").setAttribute("aria-label", `QR: ${s.setup_wifi.name}`);
}

function renderEnd(s) {
  $("end").hidden = !s.end;
  if (!s.end) return;
  $("end_text").dataset.tone = s.end.tone;
  $("end_text").setAttribute("role", s.end.tone === "bad" ? "alert" : "status");
  $("end_text").replaceChildren(...withCode(pick(s.end.text), s.end.code));
  $("end_link").hidden = !s.end.link;
  if (s.end.link) {
    $("end_link").replaceChildren(el("a", { class: "button primary", href: s.end.link, target: "_blank", rel: "noopener", text: t("open_app") }));
  }
  $("end_keep").hidden = !s.end.keep;
}

function renderLog(s) {
  $("log_card").hidden = s.view === "start" && !s.log.length;
  const pre = $("log");
  const follow = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
  for (const line of s.log) {
    pre.append(el("div", { class: line.bad ? "bad" : "", text: `${line.at}  ${line.text}` }));
  }
  if (follow) pre.scrollTop = pre.scrollHeight;
}

// fresh: a new state from the installer, whose log lines are new.
function render(s, fresh = true) {
  state = s;
  document.documentElement.lang = LANG;
  for (const node of document.querySelectorAll("[data-t]")) node.textContent = t(node.dataset.t);
  renderStart(s);
  renderRun(s);
  renderQuestion(s);
  renderSetupWifi(s);
  renderEnd(s);
  if (fresh) renderLog(s);
}

// The owner's input.

$("wifi_key_show").addEventListener("click", () => {
  const input = $("wifi_key");
  input.type = input.type === "password" ? "text" : "password";
  $("wifi_key_show").textContent = t(input.type === "password" ? "show" : "hide");
  $("wifi_key_show").dataset.t = input.type === "password" ? "show" : "hide";
});

$("setup_key_show").addEventListener("click", async () => {
  const shown = $("setup_key").dataset.shown === "1";
  if (shown) {
    $("setup_key").textContent = "••••••••";
    $("setup_key").dataset.shown = "0";
    $("setup_key_show").dataset.t = "show";
  } else {
    const key = PREVIEW ? "beispiel-schluessel-123" : (await post("setup_key", {}))?.key;
    if (!key) return;
    $("setup_key").textContent = key;
    $("setup_key").dataset.shown = "1";
    $("setup_key_show").dataset.t = "hide";
  }
  $("setup_key_show").textContent = t($("setup_key_show").dataset.t);
});

$("go").addEventListener("click", () => {
  const params = { goal, store: $("store").value.trim() };
  if (goal === "install") {
    params.wifi_name = $("wifi_name").value;
    params.wifi_key = $("wifi_key").value;
  }
  send("start", params);
});

$("log_copy").addEventListener("click", async (event) => {
  event.preventDefault();
  try {
    await navigator.clipboard.writeText($("log").innerText);
    $("log_copy").textContent = t("copied");
    setTimeout(() => ($("log_copy").textContent = t("copy")), 1500);
  } catch {
    // Without the clipboard the owner selects the text by hand.
  }
});

// Talking to the installer: every request goes under the random path the page came from.

async function post(what, params) {
  const answer = await fetch(what, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(params),
    cache: "no-store",
  });
  return answer.ok ? answer.json() : null;
}

async function send(what, params) {
  if (sending) return;
  sending = true;
  render(state, false);
  try {
    if (PREVIEW) window.previewAnswer?.(what, params);
    else await post(what, params);
  } finally {
    sending = false;
  }
  render(state, false);
  if (!PREVIEW) await poll();
}

async function poll() {
  try {
    const answer = await fetch(`state?since=${logFrom}`, { cache: "no-store" });
    if (!answer.ok) throw new Error(answer.status);
    const s = await answer.json();
    logFrom = s.log_next;
    render(s);
  } catch {
    render({ view: "gone", steps: [], log: [], end: { tone: "bad", text: t("lost") } });
    return false;
  }
  return true;
}

async function loop() {
  while (await poll()) {
    if (state.closed) return;
    await new Promise((done) => setTimeout(done, 700));
  }
}

if (PREVIEW) {
  const script = document.createElement("script");
  script.src = "preview.js";
  document.body.append(script);
} else {
  loop();
}
