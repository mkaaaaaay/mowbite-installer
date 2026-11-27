//! The page of the installer: a small HTTP server on 127.0.0.1 with a random port, everything under a random path.
//! It serves the page and its files, the state as JSON and takes the owner's input; only requests that name this
//! host and come from this page are served (other sites, DNS rebinding). It ends with the installer.

use crate::run::{Goal, Step};
use crate::secret::Secret;
use crate::text::{Ask, Owner, back_question, word};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

/// Requests are small: the start with the Wi-Fi is the largest.
const MAX_BODY: usize = 16 * 1024;
const MAX_HEAD: usize = 8 * 1024;
/// A client that sends nothing for this long is dropped.
const IDLE: Duration = Duration::from_secs(10);
/// Log lines the page gets at most per request.
const LOG_BATCH: usize = 500;

const FILES: &[(&str, &str, &[u8])] = &[
    (
        "",
        "text/html; charset=utf-8",
        include_bytes!("../web/index.html"),
    ),
    (
        "style.css",
        "text/css; charset=utf-8",
        include_bytes!("../web/style.css"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/app.js"),
    ),
    (
        "icon-192.png",
        "image/png",
        include_bytes!("../web/icon-192.png"),
    ),
    (
        "fonts/geist-sans.woff2",
        "font/woff2",
        include_bytes!("../web/fonts/geist-sans.woff2"),
    ),
    (
        "fonts/geist-mono.woff2",
        "font/woff2",
        include_bytes!("../web/fonts/geist-mono.woff2"),
    ),
    // The license of Geist goes with its fonts.
    (
        "fonts/OFL.txt",
        "text/plain; charset=utf-8",
        include_bytes!("../web/fonts/OFL.txt"),
    ),
];

/// Texts come in the language of the installer (text::word); the page shows them as they are.
type Text = String;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Note {
    pub tone: &'static str,
    pub text: Text,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The address of the app on the mower.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Remind to keep the backup folder.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keep: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub text: Text,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Answer {
    pub value: &'static str,
    pub label: Text,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<Text>,
}

/// A question the run waits on. Its answers carry the same weight; each names what it does.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Question {
    pub id: u64,
    pub tone: &'static str,
    pub text: Text,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub findings: Vec<Finding>,
    pub answers: Vec<Answer>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StepState {
    pub id: &'static str,
    pub state: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SetupWifi {
    pub name: String,
    pub qr: String,
}

/// A folder picked in the dialog of the system; n counts the picks, so the page takes each once.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Picked {
    pub n: u64,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Line {
    pub at: String,
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bad: bool,
}

/// What the page shows; the server sends it whole, the log from where the page is.
#[derive(Clone, Debug, Default, Serialize)]
pub struct View {
    pub lang: &'static str,
    pub view: &'static str,
    pub goal: &'static str,
    /// What the owner reads before a goal, by goal_id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub before: BTreeMap<&'static str, Text>,
    pub store: String,
    /// What the mower needs, for the free space the page shows at the backup folder.
    #[serde(skip)]
    pub space: crate::mower::Space,
    /// The folder dialog of the system is open.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub picking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picked: Option<Picked>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_error: Option<Note>,
    pub steps: Vec<StepState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Text>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<Question>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guide: Option<Vec<Text>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_wifi: Option<SetupWifi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<Note>,
    pub closed: bool,
    #[serde(skip)]
    pub log: Vec<Line>,
}

/// What the owner sent from the page: the start of a goal or the answer to a question.
#[derive(Debug, PartialEq)]
pub enum Input {
    Start(Value),
    Answer { question: u64, answer: String },
}

#[derive(Default)]
struct Inbox {
    inputs: Vec<Input>,
    /// The key of the setup Wi-Fi, shown only when the owner asks for it.
    setup_key: Option<String>,
}

/// The state shared between the run and the server thread.
#[derive(Default)]
pub struct Shared {
    view: Mutex<View>,
    inbox: Mutex<Inbox>,
    arrived: Condvar,
    /// Opens the folder dialog in a thread of its own, so the page goes on polling meanwhile.
    picker: OnceLock<SyncSender<PathBuf>>,
}

impl Shared {
    pub fn update(&self, change: impl FnOnce(&mut View)) {
        change(&mut self.view.lock().unwrap());
    }

    /// A line of the log, on the page and in the terminal the installer runs in.
    pub fn log(&self, text: &str, bad: bool) {
        let at = clock();
        println!("{at}  {text}");
        self.update(|v| {
            v.log.push(Line {
                at,
                text: text.to_owned(),
                bad,
            })
        });
    }

    pub fn set_setup_key(&self, key: Option<String>) {
        self.inbox.lock().unwrap().setup_key = key;
    }

    /// Waits for the next input of the owner; None after `limit`.
    pub fn next(&self, limit: Option<Duration>) -> Option<Input> {
        let mut inbox = self.inbox.lock().unwrap();
        loop {
            if !inbox.inputs.is_empty() {
                return Some(inbox.inputs.remove(0));
            }
            match limit {
                None => inbox = self.arrived.wait(inbox).unwrap(),
                Some(limit) => {
                    let (next, timeout) = self.arrived.wait_timeout(inbox, limit).unwrap();
                    inbox = next;
                    if timeout.timed_out() && inbox.inputs.is_empty() {
                        return None;
                    }
                }
            }
        }
    }

    fn push(&self, input: Input) {
        self.inbox.lock().unwrap().inputs.push(input);
        self.arrived.notify_all();
    }

    fn state(&self, since: usize) -> Vec<u8> {
        let view = self.view.lock().unwrap();
        let from = since.min(view.log.len());
        let to = (from + LOG_BATCH).min(view.log.len());
        let mut json = serde_json::to_value(&*view).unwrap_or(Value::Null);
        json["log"] = serde_json::to_value(&view.log[from..to]).unwrap_or(Value::Null);
        json["log_next"] = to.into();
        serde_json::to_vec(&json).unwrap_or_default()
    }
}

/// The time since the start of the installer for the log: no time zone to get wrong.
fn clock() -> String {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let secs = START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs();
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

pub struct Server {
    pub port: u16,
    key: String,
    pub shared: Arc<Shared>,
}

impl Server {
    /// Listens on 127.0.0.1 with a port the system picks and serves in a thread of its own until the process ends.
    pub fn start(shared: Arc<Shared>) -> Result<Server> {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("no local port for the page")?;
        let port = listener.local_addr()?.port();
        let key = crate::secret::random_hex(16)?;
        let server = Server {
            port,
            key: key.clone(),
            shared: shared.clone(),
        };
        let (picker, picks) = sync_channel::<PathBuf>(1);
        let _ = shared.picker.set(picker);
        let picked = shared.clone();
        std::thread::spawn(move || {
            for from in picks {
                let folder = pick_folder(&from);
                picked.update(|v| {
                    v.picking = false;
                    if let Some(folder) = folder {
                        v.picked = Some(Picked {
                            n: v.picked.as_ref().map_or(1, |p| p.n + 1),
                            path: crate::cli::store_in(&folder).display().to_string(),
                        });
                    }
                });
            }
        });
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = serve(stream, port, &key, &shared);
            }
        });
        Ok(server)
    }

    /// The address of the page; whoever has it can use the installer, so it goes nowhere but to the browser.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}/", self.port, self.key)
    }
}

/// The folder dialog of the system, opened at the nearest folder of from that exists. None when cancelled or when the
/// system has no dialog (Linux without portal and zenity): the owner types the folder then.
fn pick_folder(from: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(word("frame.pick_store"));
    if let Some(dir) = from.ancestors().find(|d| d.is_absolute() && d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder()
}

struct Request {
    method: String,
    path: String,
    host: Option<String>,
    origin: Option<String>,
    fetch_site: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> Result<Request> {
    stream.set_read_timeout(Some(IDLE))?;
    let mut reader = BufReader::new(stream.take((MAX_HEAD + MAX_BODY) as u64));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let (mut host, mut origin, mut fetch_site, mut length) = (None, None, None, 0usize);
    let mut head = line.len();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        head += line.len();
        anyhow::ensure!(head <= MAX_HEAD, "head too long");
        let text = line.trim_end();
        if text.is_empty() {
            break;
        }
        let Some((name, value)) = text.split_once(':') else {
            anyhow::bail!("bad header");
        };
        let value = value.trim().to_owned();
        match name.to_ascii_lowercase().as_str() {
            "host" => host = Some(value),
            "origin" => origin = Some(value),
            "sec-fetch-site" => fetch_site = Some(value),
            "content-length" => length = value.parse().context("bad length")?,
            "transfer-encoding" => anyhow::bail!("no transfer encoding"),
            _ => {}
        }
    }
    anyhow::ensure!(length <= MAX_BODY, "body too long");
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Request {
        method,
        path,
        host,
        origin,
        fetch_site,
        body,
    })
}

struct Reply {
    status: &'static str,
    kind: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn status(status: &'static str) -> Self {
        Reply {
            status,
            kind: "text/plain; charset=utf-8",
            body: status.as_bytes().to_vec(),
        }
    }

    fn json(body: Vec<u8>) -> Self {
        Reply {
            status: "200 OK",
            kind: "application/json",
            body,
        }
    }
}

fn serve(mut stream: TcpStream, port: u16, key: &str, shared: &Shared) -> Result<()> {
    let reply = match read_request(&stream) {
        Ok(request) => answer(&request, port, key, shared),
        Err(_) => Reply::status("400 Bad Request"),
    };
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n\
         Content-Security-Policy: default-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
         form-action 'none'\r\nConnection: close\r\n\r\n",
        reply.status,
        reply.kind,
        reply.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    Ok(())
}

/// Equal without telling by its time how much of it matched.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn answer(request: &Request, port: u16, key: &str, shared: &Shared) -> Reply {
    // Only this host by its address: a name that points here (DNS rebinding) does not get through.
    if request.host.as_deref() != Some(&format!("127.0.0.1:{port}")) {
        return Reply::status("421 Misdirected Request");
    }
    let page = format!("http://127.0.0.1:{port}");
    if request.origin.as_deref().is_some_and(|o| o != page)
        || request
            .fetch_site
            .as_deref()
            .is_some_and(|s| s != "same-origin" && s != "none")
    {
        return Reply::status("403 Forbidden");
    }
    let Some(rest) = request.path.strip_prefix('/') else {
        return Reply::status("404 Not Found");
    };
    let (given, rest) = rest.split_once('/').unwrap_or((rest, ""));
    if !same(given, key) {
        return Reply::status("404 Not Found");
    }
    let (name, query) = rest.split_once('?').unwrap_or((rest, ""));
    match request.method.as_str() {
        "GET" => {
            if name == "state" {
                let since = query
                    .split('&')
                    .find_map(|p| p.strip_prefix("since="))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                return Reply::json(shared.state(since));
            }
            match FILES.iter().find(|(path, _, _)| *path == name) {
                Some((_, kind, body)) => Reply {
                    status: "200 OK",
                    kind,
                    body: body.to_vec(),
                },
                None => Reply::status("404 Not Found"),
            }
        }
        // Changes only from this page: a POST always carries its origin.
        "POST" if request.origin.as_deref() == Some(page.as_str()) => {
            let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
                return Reply::status("400 Bad Request");
            };
            match name {
                "start" => shared.push(Input::Start(body)),
                "answer" => {
                    let (Some(question), Some(answer)) =
                        (body["question"].as_u64(), body["answer"].as_str())
                    else {
                        return Reply::status("400 Bad Request");
                    };
                    shared.push(Input::Answer {
                        question,
                        answer: answer.to_owned(),
                    });
                }
                "pick_folder" => {
                    let Some(picker) = shared.picker.get() else {
                        return Reply::status("404 Not Found");
                    };
                    let from = PathBuf::from(body["from"].as_str().unwrap_or_default().trim());
                    let mut open = false;
                    shared.update(|v| {
                        open = v.view == "start" && !v.picking;
                        v.picking |= open;
                    });
                    if !open {
                        return Reply::status("409 Conflict");
                    }
                    if picker.try_send(from).is_err() {
                        shared.update(|v| v.picking = false);
                        return Reply::status("409 Conflict");
                    }
                }
                "space" => {
                    let Some(goal) = goal_of(body["goal"].as_str().unwrap_or_default()) else {
                        return Reply::status("400 Bad Request");
                    };
                    let store = PathBuf::from(body["path"].as_str().unwrap_or_default().trim());
                    let need = crate::cli::needs(goal, shared.view.lock().unwrap().space);
                    let work = store.join("work");
                    let room =
                        crate::space::rooms(&store, need.backup_bytes, &work, need.work_bytes);
                    return match room.as_deref() {
                        Ok([room]) if store.is_absolute() => Reply::json(
                            serde_json::to_vec(
                                &serde_json::json!({ "free": room.free, "need": room.need }),
                            )
                            .unwrap(),
                        ),
                        _ => Reply::status("404 Not Found"),
                    };
                }
                "setup_key" => {
                    let key = shared.inbox.lock().unwrap().setup_key.clone();
                    return match key {
                        Some(key) => Reply::json(
                            serde_json::to_vec(&serde_json::json!({ "key": key })).unwrap(),
                        ),
                        None => Reply::status("404 Not Found"),
                    };
                }
                _ => return Reply::status("404 Not Found"),
            }
            Reply::json(b"{}".to_vec())
        }
        _ => Reply::status("405 Method Not Allowed"),
    }
}

/// The QR code that joins a Wi-Fi (WIFI: format), black on white as an SVG.
pub fn wifi_qr(name: &str, key: &str) -> Result<String> {
    let escape = |s: &str| {
        s.chars().fold(String::new(), |mut out, c| {
            if matches!(c, '\\' | ';' | ',' | ':' | '"') {
                out.push('\\');
            }
            out.push(c);
            out
        })
    };
    let text = format!("WIFI:T:WPA;S:{};P:{};;", escape(name), escape(key));
    let code = qrcode::QrCode::new(text.as_bytes()).context("no QR code")?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build()
        .split_once("?>")
        .map_or_else(String::new, |(_, svg)| svg.to_owned()))
}

/// The steps the page lists for a goal.
fn steps_of(goal: Goal) -> &'static [Step] {
    match goal {
        Goal::Install => &[
            Step::Connect,
            Step::Backup,
            Step::Check,
            Step::Agree,
            Step::Prepare,
            Step::Write,
            Step::FirstStart,
        ],
        Goal::Check => &[Step::Connect, Step::Backup, Step::Check],
        Goal::Back => &[Step::Connect, Step::Write],
    }
}

fn step_id(step: Step) -> &'static str {
    match step {
        Step::Connect => "connect",
        Step::Backup => "backup",
        Step::Check => "check",
        Step::Agree => "agree",
        Step::Prepare => "prepare",
        Step::Write => "write",
        Step::FirstStart => "first_start",
        Step::Done => "done",
        Step::Stopped => "stopped",
    }
}

pub fn goal_of(id: &str) -> Option<Goal> {
    [Goal::Install, Goal::Check, Goal::Back]
        .into_iter()
        .find(|g| goal_id(*g) == id)
}

pub fn goal_id(goal: Goal) -> &'static str {
    match goal {
        Goal::Install => "install",
        Goal::Check => "check",
        Goal::Back => "back",
    }
}

/// The owner on the page: says into the log, shows the step, asks with buttons and waits for the click.
pub struct PageOwner {
    shared: Arc<Shared>,
    goal: Goal,
    texts: &'static [(&'static str, &'static str, &'static str)],
    at: std::cell::Cell<Option<Step>>,
    /// The furthest step of the list reached: those before it are done.
    reached: std::cell::Cell<usize>,
    question: std::cell::Cell<u64>,
    error: std::cell::RefCell<Option<String>>,
    declined: std::cell::Cell<bool>,
    /// Where the page leads after the end, when the mower was found.
    pub app: std::cell::RefCell<Option<String>>,
}

impl PageOwner {
    pub fn new(
        shared: Arc<Shared>,
        goal: Goal,
        texts: &'static [(&'static str, &'static str, &'static str)],
    ) -> Self {
        let owner = PageOwner {
            shared,
            goal,
            texts,
            at: Default::default(),
            reached: Default::default(),
            question: Default::default(),
            error: Default::default(),
            declined: Default::default(),
            app: Default::default(),
        };
        owner.shared.update(|v| {
            v.view = "run";
            v.goal = goal_id(goal);
            v.start_error = None;
        });
        owner.mark("running");
        owner
    }

    /// The list of steps with the current one in `state`.
    fn mark(&self, state: &'static str) {
        let list = steps_of(self.goal);
        let current = self
            .at
            .get()
            .and_then(|at| list.iter().position(|s| *s == at));
        let reached = self.reached.get();
        let steps = list
            .iter()
            .enumerate()
            .map(|(i, step)| StepState {
                id: step_id(*step),
                state: if Some(i) == current || (current.is_none() && i == reached) {
                    state
                } else if i < reached {
                    "ok"
                } else {
                    "open"
                },
            })
            .collect();
        self.shared.update(|v| v.steps = steps);
    }

    /// Shows a question and waits for one of its answers.
    fn ask(
        &self,
        tone: &'static str,
        text: String,
        findings: Vec<String>,
        answers: Vec<Answer>,
    ) -> Option<&'static str> {
        let id = self.question.get() + 1;
        self.question.set(id);
        let values: Vec<&'static str> = answers.iter().map(|a| a.value).collect();
        self.shared.update(|v| {
            v.question = Some(Question {
                id,
                tone,
                text,
                code: None,
                findings: findings
                    .into_iter()
                    .map(|text| Finding {
                        text,
                        details: None,
                    })
                    .collect(),
                answers,
            });
        });
        self.mark(if tone == "bad" { "warn" } else { "waiting" });
        let chosen = loop {
            match self.shared.next(None) {
                Some(Input::Answer { question, answer }) if question == id => {
                    if let Some(value) = values.iter().find(|v| **v == answer) {
                        break Some(*value);
                    }
                }
                Some(_) => {}
                None => break None,
            }
        };
        self.shared.update(|v| v.question = None);
        self.mark("running");
        chosen
    }

    fn answer(value: &'static str, label: &str, hint: Option<&str>) -> Answer {
        Answer {
            value,
            label: label.to_owned(),
            hint: hint.map(str::to_owned),
        }
    }
}

impl Owner for PageOwner {
    fn say(&self, text: &str) {
        self.shared.log(text, false);
    }

    fn at(&self, step: Step) {
        let list = steps_of(self.goal);
        if let Some(i) = list.iter().position(|s| *s == step)
            && i > self.reached.get()
        {
            self.reached.set(i);
        }
        self.at.set(Some(step));
        self.shared.update(|v| {
            v.progress = None;
            if step != Step::Connect {
                v.guide = None;
            }
        });
        self.mark("running");
    }

    fn guide(&self, lines: &[String]) {
        self.shared.update(|v| v.guide = Some(lines.to_vec()));
    }

    fn error(&self, text: &str) {
        self.shared
            .log(&format!("{}: {text}", word("frame.error")), true);
        *self.error.borrow_mut() = Some(text.to_owned());
    }

    fn agree(&self, ask: &Ask) -> bool {
        let answers = vec![
            Self::answer("no", ask.no, None),
            Self::answer("yes", ask.yes, None),
        ];
        let yes = self.ask("warn", ask.text.clone(), ask.findings.clone(), answers) == Some("yes");
        self.declined.set(!yes);
        yes
    }

    fn back(&self, goal: Goal) -> Option<bool> {
        let text = self.error.borrow_mut().take().map_or_else(
            || word(back_question(goal)).to_owned(),
            |error| format!("{} {error}", word(back_question(goal))),
        );
        let answers = vec![
            Self::answer(
                "back",
                word("frame.answer_back"),
                Some(word("frame.hint_back")),
            ),
            Self::answer(
                "write",
                word("frame.answer_write_again"),
                Some(word("frame.hint_write_again")),
            ),
        ];
        self.ask("bad", text, vec![], answers).map(|a| a == "back")
    }

    fn again(&self) -> bool {
        let text = match self.error.borrow_mut().take() {
            Some(error) => format!("{error} {}", word("frame.again")),
            None => word("frame.again").to_owned(),
        };
        let answers = vec![
            Self::answer("stop", word("frame.answer_stop"), None),
            Self::answer("again", word("frame.answer_again"), None),
        ];
        self.ask("bad", text, vec![], answers) == Some("again")
    }

    fn setup_wifi(&self, name: &str, key: &Secret<String>, _sheet: &Path) {
        let qr = wifi_qr(name, key.expose()).unwrap_or_default();
        self.shared.set_setup_key(Some(key.expose().clone()));
        self.shared.update(|v| {
            v.setup_wifi = Some(SetupWifi {
                name: name.to_owned(),
                qr,
            })
        });
    }

    fn end(&self, ok: bool, text: &str) {
        self.shared.log(text, !ok);
        self.mark(if ok {
            "ok"
        } else if self.declined.get() {
            "declined"
        } else {
            "failed"
        });
        if ok {
            self.shared.update(|v| {
                for step in &mut v.steps {
                    step.state = "ok";
                }
            });
        }
        let error = self.error.borrow_mut().take();
        let link = self
            .app
            .borrow()
            .clone()
            .filter(|_| ok && self.goal == Goal::Install);
        self.shared.set_setup_key(None);
        self.shared.update(|v| {
            v.question = None;
            v.guide = None;
            v.setup_wifi = None;
            v.progress = None;
            v.detail = None;
            v.end = Some(Note {
                tone: if ok { "good" } else { "bad" },
                text: match error {
                    Some(error) if !ok => format!("{error} {text}"),
                    _ => text.to_owned(),
                },
                code: None,
                link,
                keep: self.goal != Goal::Back,
            });
        });
    }
}

impl crate::mower::Progress for PageOwner {
    fn step(&self, code: &'static str) {
        let text = crate::text::step_text(self.texts, code);
        self.shared.log(code, false);
        self.shared.update(|v| {
            v.detail = (text != code).then(|| text.to_owned());
            v.progress = None;
        });
    }

    fn bytes(&self, done: u64, total: u64) {
        self.shared
            .update(|v| v.progress = Some(Progress { done, total }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, path: &str, host: &str, origin: Option<&str>, body: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            host: Some(host.into()),
            origin: origin.map(Into::into),
            fetch_site: None,
            body: body.as_bytes().to_vec(),
        }
    }

    const KEY: &str = "0123456789abcdef0123456789abcdef";
    const HOST: &str = "127.0.0.1:4000";
    const PAGE: &str = "http://127.0.0.1:4000";

    fn status(r: &Request, shared: &Shared) -> &'static str {
        answer(r, 4000, KEY, shared).status
    }

    #[test]
    fn only_its_key_its_host_and_its_page() {
        let shared = Shared::default();
        let page = format!("/{KEY}/");
        assert_eq!(
            status(&request("GET", &page, HOST, None, ""), &shared),
            "200 OK"
        );
        assert_eq!(
            status(
                &request("GET", &format!("/{KEY}/app.js"), HOST, None, ""),
                &shared
            ),
            "200 OK"
        );
        assert_eq!(
            status(&request("GET", "/", HOST, None, ""), &shared),
            "404 Not Found"
        );
        assert_eq!(
            status(&request("GET", "/0123/", HOST, None, ""), &shared),
            "404 Not Found"
        );
        assert_eq!(
            status(
                &request("GET", &format!("/{KEY}/../x"), HOST, None, ""),
                &shared
            ),
            "404 Not Found"
        );
        // A name that points here, and other ports.
        for host in [
            "evil.example:4000",
            "localhost:4000",
            "127.0.0.1:4001",
            "127.0.0.1",
        ] {
            assert_eq!(
                status(&request("GET", &page, host, None, ""), &shared),
                "421 Misdirected Request"
            );
        }
        // Other sites may not read nor change.
        let other = request(
            "GET",
            &format!("/{KEY}/state"),
            HOST,
            Some("http://evil.example"),
            "",
        );
        assert_eq!(status(&other, &shared), "403 Forbidden");
        let mut cross = request("GET", &format!("/{KEY}/state"), HOST, None, "");
        cross.fetch_site = Some("cross-site".into());
        assert_eq!(status(&cross, &shared), "403 Forbidden");
        let post = |origin| {
            request(
                "POST",
                &format!("/{KEY}/answer"),
                HOST,
                origin,
                r#"{"question":1,"answer":"yes"}"#,
            )
        };
        assert_eq!(status(&post(None), &shared), "405 Method Not Allowed");
        assert_eq!(
            status(&post(Some("http://evil.example")), &shared),
            "403 Forbidden"
        );
        assert!(shared.inbox.lock().unwrap().inputs.is_empty());
        assert_eq!(status(&post(Some(PAGE)), &shared), "200 OK");
        assert_eq!(
            shared.next(Some(Duration::ZERO)),
            Some(Input::Answer {
                question: 1,
                answer: "yes".into()
            })
        );
    }

    #[test]
    fn the_state_with_the_log_from_where_the_page_is() {
        let shared = Shared::default();
        shared.update(|v| v.view = "run");
        for n in 0..3 {
            shared.log(&format!("line {n}"), n == 2);
        }
        let state: Value = serde_json::from_slice(&shared.state(1)).unwrap();
        assert_eq!(state["view"], "run");
        assert_eq!(state["log_next"], 3);
        let lines: Vec<&str> = state["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["text"].as_str().unwrap())
            .collect();
        assert_eq!(lines, ["line 1", "line 2"]);
        assert_eq!(state["log"][1]["bad"], true);
        let state: Value = serde_json::from_slice(&shared.state(99)).unwrap();
        assert_eq!(
            (state["log"].as_array().unwrap().len(), &state["log_next"]),
            (0, &Value::from(3))
        );
    }

    #[test]
    fn the_setup_key_only_on_request_from_the_page() {
        let shared = Shared::default();
        let ask = request("POST", &format!("/{KEY}/setup_key"), HOST, Some(PAGE), "{}");
        assert_eq!(status(&ask, &shared), "404 Not Found");
        shared.set_setup_key(Some("k3y".into()));
        let reply = answer(&ask, 4000, KEY, &shared);
        assert_eq!(
            serde_json::from_slice::<Value>(&reply.body).unwrap()["key"],
            "k3y"
        );
        // Never in the state.
        assert!(!String::from_utf8(shared.state(0)).unwrap().contains("k3y"));
    }

    #[test]
    fn the_folder_dialog_once_at_a_time_and_only_at_the_start() {
        let shared = Shared::default();
        let pick = request(
            "POST",
            &format!("/{KEY}/pick_folder"),
            HOST,
            Some(PAGE),
            r#"{"from":" /home/x/MowBite "}"#,
        );
        // Without a server there is no dialog.
        assert_eq!(status(&pick, &shared), "404 Not Found");
        let (picker, picks) = sync_channel(1);
        shared.picker.set(picker).unwrap();
        shared.update(|v| v.view = "start");
        assert_eq!(status(&pick, &shared), "200 OK");
        assert_eq!(picks.try_recv().unwrap(), PathBuf::from("/home/x/MowBite"));
        assert!(shared.view.lock().unwrap().picking);
        assert_eq!(status(&pick, &shared), "409 Conflict");
        assert!(picks.try_recv().is_err());
        shared.update(|v| {
            v.picking = false;
            v.view = "run";
        });
        assert_eq!(status(&pick, &shared), "409 Conflict");
        // Like every change, only from this page.
        let mut other = pick;
        other.origin = Some("http://evil.example".into());
        shared.update(|v| v.view = "start");
        assert_eq!(status(&other, &shared), "403 Forbidden");
        assert!(picks.try_recv().is_err());
    }

    #[test]
    fn the_free_space_at_a_folder_for_a_goal() {
        let shared = Shared::default();
        let space = crate::mower::Space {
            backup_bytes: 30,
            work_bytes: 12,
        };
        shared.update(|v| v.space = space);
        let dir = tempfile::tempdir().unwrap();
        let ask = |goal: &str, path: &std::path::Path| {
            let body = serde_json::json!({ "goal": goal, "path": path }).to_string();
            answer(
                &request("POST", &format!("/{KEY}/space"), HOST, Some(PAGE), &body),
                4000,
                KEY,
                &shared,
            )
        };
        let folder = dir.path().join("new");
        for (goal, need) in [("install", 42), ("check", 30), ("back", 0)] {
            let reply = ask(goal, &folder);
            assert_eq!(reply.status, "200 OK");
            let room: Value = serde_json::from_slice(&reply.body).unwrap();
            assert_eq!(room["need"], need, "{goal}");
            assert!(room["free"].as_u64().unwrap() > 0);
        }
        assert_eq!(ask("other", &folder).status, "400 Bad Request");
        assert_eq!(
            ask("install", std::path::Path::new("relative")).status,
            "404 Not Found"
        );
    }

    #[test]
    fn a_wifi_qr_is_an_svg_black_on_white() {
        let svg = wifi_qr("mowbite-7f3a", "a;b").unwrap();
        assert!(
            svg.starts_with("<svg") && svg.contains("#000000") && svg.contains("#ffffff"),
            "{svg}"
        );
    }

    #[test]
    fn a_real_request_over_the_socket() {
        let shared = Arc::new(Shared::default());
        let server = Server::start(shared).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        let path = server
            .url()
            .split_once(&format!(":{}", server.port))
            .unwrap()
            .1
            .to_owned();
        write!(
            stream,
            "GET {path}state HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.port
        )
        .unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(
            text.contains("Content-Security-Policy") && text.contains("\"log_next\":0"),
            "{text}"
        );
    }
}
