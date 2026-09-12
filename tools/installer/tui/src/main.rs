use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    backend::TestBackend,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Gauge, List, ListItem, ListState, Padding, Paragraph, Wrap,
    },
};
use serde::Deserialize;
#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt};
#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::{net::UnixStream, process::CommandExt},
};
use std::{
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
use zeroize::Zeroize;

const BG: Color = Color::Rgb(10, 13, 22);
const PANEL: Color = Color::Rgb(17, 23, 36);
const RAIL: Color = Color::Rgb(43, 55, 76);
const CYAN: Color = Color::Rgb(96, 229, 231);
const VIOLET: Color = Color::Rgb(188, 158, 255);
const TEXT: Color = Color::Rgb(226, 234, 245);
const MUTED: Color = Color::Rgb(141, 158, 182);
const GREEN: Color = Color::Rgb(123, 225, 169);
const RED: Color = Color::Rgb(255, 133, 149);

#[derive(Clone, Deserialize)]
struct Choice {
    value: String,
    label: String,
    #[serde(default)]
    detail: String,
}
#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Message {
    State {
        step: usize,
        steps: Vec<String>,
        detail: String,
        action: String,
        result: String,
        log_path: String,
        measurement: Option<(u64, u64, f64)>,
    },
    Prompt {
        id: u64,
        title: String,
        kind: String,
        options: Vec<Choice>,
    },
    Finished {
        code: i32,
    },
}
struct Prompt {
    id: u64,
    title: String,
    kind: String,
    options: Vec<Choice>,
    selected: usize,
    input: String,
}
impl Prompt {
    fn key(&mut self, key: KeyEvent) -> Option<Option<String>> {
        let mut response = None;
        match key.code {
            KeyCode::Esc => response = Some(None),
            KeyCode::Up | KeyCode::BackTab if self.kind == "choice" => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Tab if self.kind == "choice" => {
                self.selected = (self.selected + 1).min(self.options.len().saturating_sub(1));
            }
            KeyCode::Home if self.kind == "choice" => self.selected = 0,
            KeyCode::End if self.kind == "choice" => {
                self.selected = self.options.len().saturating_sub(1)
            }
            KeyCode::PageUp if self.kind == "choice" => {
                self.selected = self.selected.saturating_sub(8)
            }
            KeyCode::PageDown if self.kind == "choice" => {
                self.selected = (self.selected + 8).min(self.options.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if self.kind == "choice" {
                    if let Some(option) = self.options.get(self.selected) {
                        response = Some(Some(option.value.clone()));
                    }
                } else {
                    response = Some(Some(std::mem::take(&mut self.input)));
                }
            }
            KeyCode::Backspace if self.kind != "choice" => {
                self.input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.zeroize()
            }
            KeyCode::Char(c)
                if self.kind != "choice"
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !c.is_control()
                    && self.input.len() + c.len_utf8() <= 4096 =>
            {
                self.input.push(c)
            }
            _ => {}
        }
        response
    }
}
impl Drop for Prompt {
    fn drop(&mut self) {
        self.input.zeroize();
    }
}
struct App {
    step: usize,
    steps: Vec<String>,
    detail: String,
    action: String,
    result: String,
    log_path: String,
    measurement: Option<(u64, u64, f64)>,
    prompt: Option<Prompt>,
    started: Instant,
    finished: Option<i32>,
    stopping: bool,
    demo: bool,
}
impl App {
    fn new(demo: bool) -> Self {
        Self {
            step: 0,
            steps: [
                "Prepare",
                "USB startup",
                "Wi-Fi",
                "Android backup",
                "Install Couch",
                "First boot",
            ]
            .map(str::to_owned)
            .into(),
            detail: "Checking the installer package…".into(),
            action: "Keep your remote connected.".into(),
            result: "active".into(),
            log_path: String::new(),
            measurement: None,
            prompt: None,
            started: Instant::now(),
            finished: None,
            stopping: false,
            demo,
        }
    }
    fn receive(&mut self, message: Message) {
        match message {
            Message::State {
                step,
                steps,
                detail,
                action,
                result,
                log_path,
                measurement,
            } => {
                self.step = step.min(steps.len().saturating_sub(1));
                self.steps = steps;
                self.detail = clean(&detail);
                self.action = clean(&action);
                self.result = result;
                self.log_path = clean(&log_path);
                self.measurement = measurement.filter(|(done, total, rate)| {
                    *total > 0 && done <= total && rate.is_finite() && *rate >= 0.0
                });
            }
            Message::Prompt {
                id,
                title,
                kind,
                options,
            } => {
                self.prompt = Some(Prompt {
                    id,
                    title: clean(&title),
                    kind,
                    options,
                    selected: 0,
                    input: String::new(),
                });
            }
            Message::Finished { code } => {
                self.prompt = None;
                self.finished = Some(code);
            }
        }
    }
    fn title(&self) -> &str {
        self.steps
            .get(self.step)
            .map(String::as_str)
            .unwrap_or("Prepare")
    }
    fn accent(&self) -> Color {
        if self.result == "error" {
            RED
        } else if self.result == "ok" {
            GREEN
        } else {
            CYAN
        }
    }
    fn demo_prompt(&mut self) {
        self.step = 2;
        self.detail = "Wi-Fi is ready.".into();
        self.action =
            "Choose the security used by your network. You'll enter the password next.".into();
        self.receive(Message::Prompt {
            id: 1,
            title: "Wi-Fi security".into(),
            kind: "choice".into(),
            options: vec![
                Choice {
                    value: "wpa2".into(),
                    label: "WPA2 Personal".into(),
                    detail: "Password-protected home networks".into(),
                },
                Choice {
                    value: "open".into(),
                    label: "Open network".into(),
                    detail: "No Wi-Fi password".into(),
                },
            ],
        });
    }
    fn demo_networks(&mut self) {
        self.demo_prompt();
        self.detail = "Nearby networks are ready.".into();
        self.action = "Choose a network, enter its password, or use manual entry.".into();
        let prompt = self.prompt.as_mut().unwrap();
        prompt.title = "Choose Wi-Fi network".into();
        prompt.options = (1..=64)
            .map(|n| Choice {
                value: format!("network-{n}"),
                label: format!("Example network {n:02} · WPA2 · -40 dBm"),
                detail: String::new(),
            })
            .collect();
        for (value, label) in [
            ("manual", "Enter network manually"),
            ("rescan", "Scan again"),
        ] {
            prompt.options.push(Choice {
                value: value.into(),
                label: label.into(),
                detail: String::new(),
            });
        }
        prompt.selected = prompt.options.len() - 1;
    }
}
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .take(8192)
        .collect()
}
fn panel(title: &str, accent: Color) -> Block<'_> {
    Block::default()
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(title, Style::default().fg(accent).bold()),
            Span::raw(" "),
        ]))
        .padding(Padding::horizontal(1))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(RAIL))
        .style(Style::default().bg(PANEL).fg(TEXT))
}
fn elapsed(app: &App) -> String {
    let s = app.started.elapsed().as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}
fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
    let area = area.inner(Margin::new(
        if area.width >= 60 { 2 } else { 0 },
        if area.height >= 20 { 1 } else { 0 },
    ));
    let compact = area.height < 19;
    let chunks = Layout::vertical([
        Constraint::Length(if compact { 1 } else { 3 }),
        Constraint::Length(if compact { 2 } else { 3 }),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    let heading = Line::from(vec![
        Span::styled("couch.", Style::default().fg(CYAN).bold()),
        Span::styled("   /   DEVICE SETUP", Style::default().fg(MUTED)),
        Span::styled(
            if app.demo {
                "   PREVIEW · NO DEVICE ACCESS"
            } else {
                ""
            },
            Style::default().fg(VIOLET),
        ),
    ]);
    frame.render_widget(Paragraph::new(heading), chunks[0]);
    let mut route = Vec::new();
    let short = ["PREPARE", "USB", "WI-FI", "BACKUP", "WRITE", "BOOT"];
    for (i, name) in short.iter().enumerate() {
        let (symbol, color) = if i < app.step {
            ("✓", GREEN)
        } else if i == app.step {
            ("●", CYAN)
        } else {
            ("○", MUTED)
        };
        route.push(Span::styled(
            format!("{symbol} {name}  "),
            Style::default().fg(color),
        ));
    }
    let line = if area.width >= 60 {
        Line::from(route)
    } else {
        Line::from(Span::styled(
            format!(
                "STEP {} / {}  ·  {}",
                app.step + 1,
                app.steps.len(),
                app.title()
            ),
            Style::default().fg(CYAN),
        ))
    };
    frame.render_widget(Paragraph::new(line), chunks[1]);
    let body = if area.width >= 110 && !compact {
        let cols = Layout::horizontal([Constraint::Min(40), Constraint::Length(28)])
            .spacing(2)
            .split(chunks[2]);
        sidebar(frame, app, cols[1]);
        cols[0]
    } else {
        chunks[2]
    };
    if let Some(prompt) = &app.prompt {
        let desired = if prompt.kind == "choice" {
            prompt
                .options
                .len()
                .saturating_mul(2)
                .saturating_add(3)
                .min(11) as u16
        } else {
            7
        };
        let prompt_height = desired
            .min(body.height.saturating_sub(if compact { 1 } else { 4 }))
            .max(1);
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(prompt_height)])
            .spacing(if compact { 0 } else { 1 })
            .split(body);
        status(frame, app, rows[0], compact);
        prompt_widget(frame, prompt, rows[1]);
    } else {
        let rows = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(if compact { 4 } else { 6 }),
        ])
        .spacing(1)
        .split(body);
        status(frame, app, rows[0], compact);
        let text = if app.finished.is_some() && !app.log_path.is_empty() {
            format!("{}\n\nLog: {}", app.action, app.log_path)
        } else {
            app.action.clone()
        };
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(panel(
                if app.result == "error" {
                    "KEEP YOUR ORIGINALS"
                } else {
                    "NEXT"
                },
                VIOLET,
            )),
            rows[1],
        );
    }
    let help = if app.finished.is_some() {
        "Enter / Esc  close"
    } else if app.prompt.as_ref().is_some_and(|p| p.kind == "choice") {
        if area.width < 60 {
            "↑↓ move · End last · Enter select"
        } else {
            "↑↓ move · PgUp/PgDn ±8 · Home/End jump · Enter select · Esc cancel"
        }
    } else if app.prompt.is_some() {
        "Enter  continue     Ctrl+U  clear     Esc  cancel"
    } else {
        "Ctrl+C  stop     Keep the remote powered and connected"
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(MUTED)),
        chunks[3],
    );
    if std::env::var_os("NO_COLOR").is_some() {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut frame.buffer_mut()[(x, y)];
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
            }
        }
    }
}
fn status(frame: &mut Frame, app: &App, area: Rect, compact: bool) {
    if area.height < 3 {
        frame.render_widget(
            Paragraph::new(app.detail.as_str()).style(Style::default().fg(TEXT)),
            area,
        );
        return;
    }
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let marker = if app.finished.is_some() {
        if app.result == "error" { "!" } else { "✓" }
    } else if app.prompt.is_some() {
        "›"
    } else {
        spinner[(app.started.elapsed().as_millis() / 90) as usize % spinner.len()]
    };
    let title = format!("{marker}  {}   ·   {}", app.title(), elapsed(app));
    let block = panel(&title, app.accent());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some((done, total, rate)) = app.measurement {
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new(app.detail.as_str()).wrap(Wrap { trim: false }),
            rows[0],
        );
        frame.render_widget(
            Gauge::default()
                .ratio(done as f64 / total as f64)
                .label(format!("{:.1}%", done as f64 / total as f64 * 100.0))
                .gauge_style(Style::default().fg(CYAN).bg(RAIL))
                .use_unicode(true),
            rows[1],
        );
        let eta = if rate > 0.0 {
            format!(
                " · ~{}s remaining",
                ((total - done) as f64 / rate).ceil() as u64
            )
        } else {
            String::new()
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{:.1} / {:.1} MiB   ·   {:.1} MiB/s{eta}",
                done as f64 / 1048576.0,
                total as f64 / 1048576.0,
                rate / 1048576.0
            ))
            .style(Style::default().fg(MUTED))
            .wrap(Wrap { trim: false }),
            rows[2],
        );
    } else {
        let mut lines = vec![Line::from(app.detail.as_str())];
        if !compact && inner.height >= 4 {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                if app.stopping {
                    "Stopping safely…"
                } else if app.prompt.is_some() {
                    "YOUR TURN"
                } else if app.finished.is_some() {
                    "SESSION ENDED"
                } else {
                    "WORKING"
                },
                Style::default().fg(VIOLET),
            )));
            if app.prompt.is_some() {
                for line in app.action.lines() {
                    lines.push(Line::from(line));
                }
            }
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}
fn prompt_widget(frame: &mut Frame, prompt: &Prompt, area: Rect) {
    let mut block = panel(&prompt.title, VIOLET).border_style(Style::default().fg(VIOLET));
    if prompt.kind == "choice" {
        // Keep position separate from the title so a long network prompt cannot
        // hide it. The bottom border costs no list rows on small terminals.
        let position = if prompt.options.is_empty() {
            0
        } else {
            prompt.selected + 1
        };
        block = block.title_bottom(
            Line::from(format!(" {position}/{} · Home/End ", prompt.options.len())).right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if prompt.kind == "choice" {
        let detailed = usize::from(inner.height) >= prompt.options.len().saturating_mul(2);
        let items = prompt
            .options
            .iter()
            .map(|o| {
                let mut lines = vec![Line::from(clean(&o.label))];
                if detailed {
                    lines.push(Line::from(Span::styled(
                        clean(&o.detail),
                        Style::default().fg(MUTED),
                    )));
                }
                ListItem::new(lines)
            })
            .collect::<Vec<_>>();
        let mut selected = ListState::default().with_selected(Some(prompt.selected));
        frame.render_stateful_widget(
            List::new(items).highlight_symbol(" › ").highlight_style(
                Style::default()
                    .bg(Color::Rgb(35, 51, 67))
                    .fg(CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            inner,
            &mut selected,
        );
    } else {
        let visible = if prompt.kind == "password" {
            "•".repeat(prompt.input.chars().count())
        } else {
            prompt.input.clone()
        };
        let tail = visible
            .chars()
            .rev()
            .take(inner.width.saturating_sub(3) as usize)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!(" › {tail}▏"),
                    Style::default().fg(CYAN),
                )),
                Line::default(),
                Line::from(Span::styled(
                    if prompt.kind == "password" {
                        "Hidden · never written to the installer log"
                    } else {
                        "Type your answer, then press Enter"
                    },
                    Style::default().fg(MUTED),
                )),
            ]),
            inner,
        );
    }
}
fn sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(9), Constraint::Min(0)]).split(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("SESSION", Style::default().fg(MUTED))),
            Line::from(elapsed(app)),
            Line::default(),
            Line::from(Span::styled("CURRENT STEP", Style::default().fg(MUTED))),
            Line::from(app.title()),
            Line::default(),
            Line::from(Span::styled(
                if app.step >= 3 {
                    "Wi-Fi transfer"
                } else {
                    "USB startup / Wi-Fi setup"
                },
                Style::default().fg(CYAN),
            )),
        ])
        .block(panel("OVERVIEW", VIOLET)),
        rows[0],
    );
}
#[cfg(windows)]
struct BackendJob(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl BackendJob {
    fn new() -> io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}
#[cfg(windows)]
impl Drop for BackendJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
struct Worker {
    #[cfg(windows)]
    _job: BackendJob,
    child: Child,
    input: Box<dyn Write + Send>,
    messages: Receiver<Result<Message, String>>,
}
impl Worker {
    fn start(args: &[String]) -> io::Result<Self> {
        let mut python = None;
        let mut backend = None;
        let mut native_backend = None;
        let mut native_smoke = false;
        let mut forwarded = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let name = &args[i];
            i += 1;
            if name == "--ui-smoke" {
                native_smoke = true;
                continue;
            }
            let value = args
                .get(i)
                .ok_or_else(|| io::Error::other("Missing argument value"))?;
            i += 1;
            match name.as_str() {
                "--python" => python = Some(value.clone()),
                "--backend" => backend = Some(value.clone()),
                "--native-backend" => native_backend = Some(value.clone()),
                "--config" | "--wifi-retry-from" | "--wifi-restore-from" => {
                    forwarded.push(name.clone());
                    forwarded.push(value.clone());
                }
                _ => return Err(io::Error::other("Unknown installer argument")),
            }
        }
        #[cfg(unix)]
        let (input, child_socket) = UnixStream::pair()?;
        #[cfg(unix)]
        let output: Box<dyn Read + Send> = Box::new(input.try_clone()?);
        #[cfg(unix)]
        let socket_fd = child_socket.as_raw_fd();
        #[cfg(target_os = "linux")]
        let parent_pid = std::process::id() as libc::pid_t;
        if native_smoke && native_backend.is_none() {
            return Err(io::Error::other("--ui-smoke requires --native-backend"));
        }
        let mut command = backend_command(python, backend, native_backend)?;
        if native_smoke {
            command.arg("--ui-smoke");
        }
        command
            .args(forwarded)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        command.args(["--events-fd", "3"]);
        #[cfg(windows)]
        command
            .arg("--events-stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(0x00000200); // CREATE_NEW_PROCESS_GROUP for safe cancellation.
        // Only this dedicated socket carries UI events/answers. Native tool output
        // cannot corrupt the protocol or paint over the terminal. No secrets in argv.
        #[cfg(unix)]
        unsafe {
            command.pre_exec(move || {
                #[cfg(target_os = "linux")]
                {
                    // No orphaned writer if the UI is killed or its terminal disappears.
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent_pid {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Installer UI exited",
                        ));
                    }
                }
                if libc::dup2(socket_fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        #[cfg(windows)]
        let job = BackendJob::new()?;
        #[allow(unused_mut)]
        let mut child = command.spawn()?;
        #[cfg(windows)]
        unsafe {
            if windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                job.0,
                child.as_raw_handle(),
            ) == 0
            {
                let error = io::Error::last_os_error();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        #[cfg(unix)]
        drop(child_socket);
        #[cfg(unix)]
        let input: Box<dyn Write + Send> = Box::new(input);
        #[cfg(windows)]
        let input: Box<dyn Write + Send> = Box::new(
            child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("Missing backend input"))?,
        );
        #[cfg(windows)]
        let output: Box<dyn Read + Send> = Box::new(
            child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("Missing backend output"))?,
        );
        let (send, messages) = mpsc::sync_channel(128);
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut raw = Vec::new();
                let read = std::io::Read::by_ref(&mut reader)
                    .take(65537)
                    .read_until(b'\n', &mut raw);
                match read {
                    Ok(0) => break,
                    Ok(n) if n <= 65536 => {
                        let event = serde_json::from_slice(&raw)
                            .map_err(|_| "Installer interface protocol error".to_owned());
                        if send.send(event).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = send.send(Err("Installer interface disconnected".into()));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            #[cfg(windows)]
            _job: job,
            child,
            input,
            messages,
        })
    }
    fn reply(&mut self, id: u64, mut value: Option<String>) -> io::Result<()> {
        let mut bytes = if let Some(text) = &value {
            serde_json::to_vec(&serde_json::json!({"id":id,"value":text}))?
        } else {
            serde_json::to_vec(&serde_json::json!({"id":id,"cancel":true}))?
        };
        bytes.push(b'\n');
        let result = self
            .input
            .write_all(&bytes)
            .and_then(|_| self.input.flush());
        bytes.zeroize();
        if let Some(text) = &mut value {
            text.zeroize();
        }
        result
    }
    fn stop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(1, self.child.id());
        }
    }
}
fn backend_command(
    python: Option<String>,
    backend: Option<String>,
    native: Option<String>,
) -> io::Result<Command> {
    match (python, backend, native) {
        (None, None, Some(executable)) => Ok(Command::new(executable)),
        (Some(python), Some(backend), None) => {
            let mut command = Command::new(python);
            command.arg("-B").arg(backend);
            Ok(command)
        }
        _ => Err(io::Error::other(
            "Use --native-backend, or both --python and --backend",
        )),
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
fn run(app: &mut App, worker: &mut Option<Worker>) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    struct TerminalGuard;
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            let _ = crossterm::execute!(io::stdout(), event::DisableBracketedPaste);
            ratatui::restore();
        }
    }
    let _terminal_guard = TerminalGuard;
    crossterm::execute!(io::stdout(), event::EnableBracketedPaste)?;
    let result = (|| {
        loop {
            if let Some(w) = worker.as_mut() {
                while let Ok(message) = w.messages.try_recv() {
                    match message {
                        Ok(m) => app.receive(m),
                        Err(e) => {
                            app.detail = e;
                            app.result = "error".into();
                            app.finished = Some(1);
                            w.stop();
                        }
                    }
                }
                if app.finished.is_none() {
                    if let Some(exit) = w.child.try_wait()? {
                        app.finished = Some(exit.code().unwrap_or(1));
                        app.result = if exit.success() { "ok" } else { "error" }.into();
                        if !exit.success() {
                            app.detail = "Installer stopped. Keep your originals and log.".into();
                        }
                    }
                }
            }
            terminal.draw(|frame| draw(frame, app))?;
            if !event::poll(Duration::from_millis(80))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if app.finished.is_some()
                        && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q'))
                    {
                        break;
                    }
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if app.finished.is_some() {
                            break;
                        }
                        if let Some(w) = worker {
                            w.stop();
                            app.stopping = true;
                        } else {
                            break;
                        }
                        continue;
                    }
                    let Some(prompt) = app.prompt.as_mut() else {
                        continue;
                    };
                    let response = prompt.key(key);
                    if let Some(value) = response {
                        let id = prompt.id;
                        if let Some(w) = worker {
                            w.reply(id, value)?;
                            app.prompt = None;
                        } else {
                            if value.is_none() {
                                break;
                            }
                            app.prompt = None;
                            app.measurement = Some((84 * 1048576, 126 * 1048576, 15.0 * 1048576.0));
                            app.step = 4;
                            app.detail = "Verify installed: userdata".into();
                            app.action =
                                "The compact image is verified before its filesystem is expanded."
                                    .into();
                        }
                    }
                }
                Event::Paste(mut text) => {
                    if let Some(prompt) = app.prompt.as_mut() {
                        if prompt.kind != "choice" {
                            for c in text.chars().filter(|c| !c.is_control()) {
                                if prompt.input.len() + c.len_utf8() <= 4096 {
                                    prompt.input.push(c);
                                }
                            }
                        }
                    }
                    text.zeroize();
                }
                _ => {}
            }
        }
        Ok(())
    })();
    result
}
fn snapshot(app: &App) -> io::Result<()> {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer();
    for y in 0..30 {
        for x in 0..100 {
            let cell = &buffer[(x, y)];
            let fg = match cell.fg {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (226, 234, 245),
            };
            let bg = match cell.bg {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (10, 13, 22),
            };
            print!(
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m{}",
                fg.0,
                fg.1,
                fg.2,
                bg.0,
                bg.1,
                bg.2,
                cell.symbol()
            );
        }
        println!("\x1b[0m");
    }
    Ok(())
}
fn main() -> io::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let network_snapshot = args == ["--snapshot-networks"];
    let demo = args == ["--demo"] || args == ["--snapshot"] || network_snapshot;
    let mut app = App::new(demo);
    if demo {
        app.demo_prompt();
    }
    if network_snapshot {
        app.demo_networks();
    }
    if args == ["--snapshot"] || network_snapshot {
        return snapshot(&app);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "Run the installer in an interactive terminal",
        ));
    }
    let mut worker = if demo {
        None
    } else {
        Some(Worker::start(&args)?)
    };
    let result = run(&mut app, &mut worker);
    drop(worker);
    println!("{}", app.detail);
    if !app.log_path.is_empty() {
        println!("Log: {}", app.log_path);
    }
    result?;
    if app.finished.is_some_and(|code| code != 0) {
        std::process::exit(app.finished.unwrap());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires COUCH_NATIVE_HOST_TEST built from the host workspace"]
    fn native_private_channel_roundtrip_and_cancel() {
        let executable = std::env::var("COUCH_NATIVE_HOST_TEST").expect("native host fixture path");
        for cancel in [false, true] {
            let mut worker = Worker::start(&[
                "--native-backend".into(),
                executable.clone(),
                "--ui-smoke".into(),
            ])
            .unwrap();
            let mut prompts = 0;
            let mut finished = false;
            for _ in 0..12 {
                let event = worker.messages.recv_timeout(Duration::from_secs(5));
                if cancel && prompts == 1 && event.is_err() {
                    break;
                }
                match event.unwrap().unwrap() {
                    Message::Prompt { id, kind, .. } => {
                        prompts += 1;
                        if prompts == 1 {
                            assert_eq!(kind, "choice");
                            worker.reply(id, (!cancel).then(|| "0".into())).unwrap();
                        } else {
                            assert_eq!(kind, "password");
                            worker
                                .reply(id, Some("fixture-private-input".into()))
                                .unwrap();
                        }
                    }
                    Message::State {
                        detail,
                        action,
                        result,
                        ..
                    } => {
                        assert!(
                            !format!("{detail}{action}{result}").contains("fixture-private-input")
                        );
                    }
                    Message::Finished { code } => {
                        assert_eq!(code, 0);
                        finished = true;
                        break;
                    }
                }
            }
            assert_eq!(finished, !cancel);
            assert_eq!(prompts, if cancel { 1 } else { 2 });
            assert_eq!(worker.child.wait().unwrap().success(), !cancel);
        }
    }
    #[test]
    fn native_backend_is_exclusive_and_has_no_python_arguments() {
        let command = super::backend_command(None, None, Some("native-host".into())).unwrap();
        assert_eq!(command.get_program(), "native-host");
        assert_eq!(command.get_args().count(), 0);
        assert!(
            super::backend_command(Some("python".into()), None, Some("native".into())).is_err()
        );
        assert!(
            super::backend_command(None, Some("script".into()), Some("native".into())).is_err()
        );
        assert!(super::backend_command(None, None, None).is_err());
    }
    use super::*;
    fn text(app: &App, width: u16, height: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    #[test]
    fn menus_fit_common_terminal_sizes() {
        let mut app = App::new(true);
        app.demo_prompt();
        for (w, h) in [(120, 36), (80, 24), (40, 12)] {
            let frame = text(&app, w, h);
            assert!(frame.contains("WPA2 Personal"));
            assert!(frame.contains("Wi-Fi security"));
        }
    }
    #[test]
    fn long_network_menu_keeps_manual_and_rescan_visible_at_end() {
        let mut app = App::new(true);
        app.demo_networks();
        for (w, h) in [(120, 36), (80, 24), (40, 12), (32, 10)] {
            let frame = text(&app, w, h);
            assert!(
                frame.contains("Enter network manually"),
                "manual missing at {w}x{h}"
            );
            assert!(
                frame.contains("› Scan again"),
                "selected last choice missing at {w}x{h}"
            );
            assert!(frame.contains("66/66"), "position missing at {w}x{h}");
            assert!(!frame.contains("Example network 01"));
        }
        // Degenerate sizes may not fit text, but resizing must never panic.
        for (w, h) in [(1, 1), (12, 4), (20, 8)] {
            let _ = text(&app, w, h);
        }
    }
    #[test]
    fn long_menu_navigation_clamps_and_submits_actual_last_option() {
        let mut app = App::new(true);
        app.demo_networks();
        let p = app.prompt.as_mut().unwrap();
        for (key, expected) in [
            (KeyCode::Home, 0),
            (KeyCode::PageUp, 0),
            (KeyCode::PageDown, 8),
            (KeyCode::PageUp, 0),
            (KeyCode::End, 65),
            (KeyCode::PageDown, 65),
            (KeyCode::Down, 65),
        ] {
            p.key(KeyEvent::new(key, KeyModifiers::NONE));
            assert_eq!(p.selected, expected);
        }
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Some("rescan".into()))
        );
        p.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Some("manual".into()))
        );
        p.options.clear();
        p.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
    }
    #[test]
    fn passwords_never_render_in_frames() {
        let mut app = App::new(true);
        app.demo_prompt();
        let p = app.prompt.as_mut().unwrap();
        p.kind = "password".into();
        p.input = "secret-example-123".into();
        let frame = text(&app, 80, 24);
        assert!(!frame.contains("secret-example"));
        assert!(frame.contains("•••"));
    }
    #[test]
    fn invalid_measurement_cannot_become_fake_progress() {
        let mut app = App::new(false);
        app.receive(Message::State {
            step: 0,
            steps: app.steps.clone(),
            detail: "test".into(),
            action: String::new(),
            result: "active".into(),
            log_path: String::new(),
            measurement: Some((11, 10, 1.0)),
        });
        assert!(app.measurement.is_none());
    }
    #[test]
    fn menu_typing_cannot_submit_an_invalid_security_value() {
        let mut app = App::new(true);
        app.demo_prompt();
        let p = app.prompt.as_mut().unwrap();
        assert!(
            p.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
                .is_none()
        );
        assert!(p.input.is_empty());
        p.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Some("open".into()))
        );
    }
    #[test]
    fn input_backspace_and_clear_work_without_echoing_passwords() {
        let mut app = App::new(true);
        app.demo_prompt();
        let p = app.prompt.as_mut().unwrap();
        p.kind = "password".into();
        p.key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::NONE));
        p.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(p.input.is_empty());
        p.input = "fixture secret".into();
        p.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(p.input.is_empty());
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(None)
        );
    }
    #[test]
    #[cfg(unix)]
    fn dedicated_socket_survives_native_stdout_and_delivers_private_answers() {
        let path =
            std::env::temp_dir().join(format!("couch-ratatui-ipc-{}.py", std::process::id()));
        std::fs::write(&path,r#"import os,json
out=os.fdopen(os.dup(3),'w',buffering=1)
incoming=os.fdopen(os.dup(3),'r')
print('unstructured native tool output',flush=True)
out.write(json.dumps({'event':'prompt','id':7,'title':'Password','kind':'password','options':[]})+'\n')
answer=json.loads(incoming.readline())
assert answer == {'id':7,'value':'fixture-secret'}
out.write(json.dumps({'event':'finished','code':0})+'\n')
"#).unwrap();
        let args = vec![
            "--python".into(),
            "python3".into(),
            "--backend".into(),
            path.to_string_lossy().into_owned(),
            "--config".into(),
            "unused".into(),
        ];
        let mut worker = Worker::start(&args).unwrap();
        assert!(matches!(
            worker
                .messages
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            Message::Prompt { id: 7, .. }
        ));
        worker.reply(7, Some("fixture-secret".into())).unwrap();
        assert!(matches!(
            worker
                .messages
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            Message::Finished { code: 0 }
        ));
        drop(worker);
        std::fs::remove_file(path).unwrap();
    }
}
