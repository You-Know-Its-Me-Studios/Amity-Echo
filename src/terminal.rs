//! Human-facing terminal presentation. The agent and host modules own decisions;
//! this module only collects input and renders their state.

use std::env;
use std::cell::RefCell;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crossterm::{execute, queue};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::state::{MessageRole, Session, SessionStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Line(String),
    Interrupt,
    Eof,
}

/// A live choice supplied by the CLI for the slash menu.
#[derive(Clone, Debug)]
pub struct MenuChoice {
    pub command: String,
    pub description: String,
}

pub struct Terminal {
    editor: Option<DefaultEditor>,
    history_path: PathBuf,
    color: bool,
    screen: Option<RefCell<Screen>>,
}

#[derive(Clone, Copy)]
enum Tone { Brand, Muted, User, Assistant, Info, Warning, Error, Operation, Tool, Code }

struct ScreenLine { text: String, tone: Tone }

#[derive(Clone)]
struct MenuItem {
    command: String,
    description: String,
    submit: bool,
}

struct Screen {
    cwd: PathBuf,
    project: String,
    session: String,
    model: String,
    lines: Vec<ScreenLine>,
    input: String,
    cursor: usize,
    scroll: usize,
    status: String,
    approval: bool,
    raw: bool,
    history: Vec<String>,
    history_position: Option<usize>,
    saved_draft: String,
    engaged: bool,
    menu_choices: Vec<MenuChoice>,
    project_connected: bool,
    menu_index: usize,
    menu_dismissed: bool,
    session_menu_loaded: bool,
}

impl Terminal {
    pub fn new(history_path: PathBuf, color: bool) -> Result<Self> {
        if let Some(parent) = history_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating terminal history directory {}", parent.display()))?;
        }
        Ok(Self {
            editor: None,
            history_path,
            color: color
                && io::stdout().is_terminal()
                && env::var_os("NO_COLOR").is_none()
                && env::var("TERM").ok().as_deref() != Some("dumb"),
            screen: None,
        })
    }

    pub fn enter_interactive(&mut self, cwd: &Path) -> Result<()> {
        if self.screen.is_some() || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Ok(());
        }
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
            return Err(error).context("opening Echo terminal screen");
        }
        self.screen = Some(RefCell::new(Screen {
            cwd: cwd.to_path_buf(), project: "Local workspace".into(), session: "new".into(),
            model: "not connected".into(), lines: Vec::new(), input: String::new(),
            cursor: 0, scroll: 0, status: "Ready".into(), approval: false, raw: false,
            history: Vec::new(), history_position: None, saved_draft: String::new(),
            engaged: false, menu_choices: Vec::new(), project_connected: false,
            menu_index: 0, menu_dismissed: false, session_menu_loaded: false,
        }));
        self.draw().context("drawing Echo terminal screen")
    }

    /// Refresh model and project choices before reading a command.
    pub fn set_menu_choices(&self, project_connected: bool, choices: Vec<MenuChoice>) {
        let Some(screen) = &self.screen else { return; };
        let mut screen = screen.borrow_mut();
        screen.project_connected = project_connected;
        screen.menu_choices = choices;
        screen.menu_index = 0;
        screen.session_menu_loaded = false;
    }

    /// Replace the visible transcript with the selected session's stored conversation.
    pub fn load_session(&self, session: &Session) {
        let Some(screen) = &self.screen else { return; };
        {
            let mut screen = screen.borrow_mut();
            screen.project = session.project_id.clone();
            screen.session = session.id.clone();
            screen.model = session.model.as_deref().unwrap_or("default").to_owned();
            screen.lines.clear();
            screen.scroll = 0;
            screen.engaged = !session.messages.is_empty();
            screen.input.clear();
            screen.cursor = 0;
            screen.history_position = None;
            screen.saved_draft.clear();
            screen.approval = false;
            screen.status = match &session.status {
                SessionStatus::Idle => "Ready",
                SessionStatus::Running => "Task running",
                SessionStatus::WaitingApproval => "Approval needed",
                SessionStatus::Interrupted => "Task interrupted",
                SessionStatus::Completed => "Task complete",
                SessionStatus::Failed => "Task failed",
                SessionStatus::Cancelled => "Task cancelled",
            }.to_owned();
            for message in &session.messages {
                match &message.role {
                    MessageRole::User => Self::append_screen_lines(
                        &mut screen, &format!("› {}", message.content), Tone::User),
                    MessageRole::Assistant => {
                        Self::append_screen_lines(&mut screen, "ECHO", Tone::Brand);
                        Self::append_screen_lines(&mut screen, &message.content, Tone::Assistant);
                        Self::append_screen_lines(&mut screen, "", Tone::Muted);
                    }
                    MessageRole::Tool => {
                        Self::append_screen_lines(&mut screen, "TOOL", Tone::Tool);
                        Self::append_screen_lines(&mut screen, &message.content, Tone::Muted);
                        Self::append_screen_lines(&mut screen, "", Tone::Muted);
                    }
                    MessageRole::System => {
                        Self::append_screen_lines(&mut screen, "SYSTEM", Tone::Warning);
                        Self::append_screen_lines(&mut screen, &message.content, Tone::Info);
                    }
                }
            }
            if screen.lines.len() > 4000 {
                let excess = screen.lines.len() - 4000;
                screen.lines.drain(..excess);
            }
        }
        let _ = self.draw();
    }

    pub fn read_line(&mut self, project: &str, session: &str,
        session_choices: &mut dyn FnMut() -> Result<Vec<MenuChoice>>) -> Result<Input> {
        if self.screen.is_some() {
            {
                let mut screen = self.screen.as_ref().expect("screen is present").borrow_mut();
                screen.project = project.to_owned();
                screen.session = session.to_owned();
            }
            return self.read_screen_input(true, Some(session_choices));
        }
        let prompt = format!(
            "{} {} {} ",
            self.paint("Echo", "1;36"),
            self.paint(&format!("[{project}]"), "36"),
            self.paint(&format!("({session}) ›"), "2")
        );
        self.read_with_prompt(&prompt, true)
    }

    pub fn ask_approval(&mut self, description: &str) -> Result<bool> {
        if self.screen.is_some() {
            self.log(description, Tone::Warning);
            self.screen.as_ref().expect("screen is present").borrow_mut().approval = true;
            loop {
                match self.read_screen_input(false, None)? {
                    Input::Line(answer) => match answer.trim().to_ascii_lowercase().as_str() {
                        "y" | "yes" => { self.screen.as_ref().expect("screen is present").borrow_mut().approval = false; return Ok(true); }
                        "" | "n" | "no" => { self.screen.as_ref().expect("screen is present").borrow_mut().approval = false; return Ok(false); }
                        _ => self.log("Enter y or n.", Tone::Info),
                    },
                    Input::Interrupt | Input::Eof => { self.screen.as_ref().expect("screen is present").borrow_mut().approval = false; return Ok(false); }
                }
            }
        }
        self.warning(description);
        loop {
            let prompt = format!("{} ", self.paint("Approve this operation? [y/N]", "1;33"));
            match self.read_with_prompt(&prompt, false)? {
                Input::Line(answer) => match answer.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" => return Ok(true),
                    "" | "n" | "no" => return Ok(false),
                    _ => self.info("Enter y or n."),
                },
                Input::Interrupt | Input::Eof => return Ok(false),
            }
        }
    }

    fn read_screen_input(&mut self, record_input: bool,
        session_choices: Option<&mut dyn FnMut() -> Result<Vec<MenuChoice>>>) -> Result<Input> {
        terminal::enable_raw_mode().context("enabling Echo keyboard input")?;
        self.screen.as_ref().expect("screen is present").borrow_mut().raw = true;
        let result = self.screen_input_loop(record_input, session_choices);
        let disable = terminal::disable_raw_mode().context("restoring normal terminal input");
        if disable.is_ok() {
            self.screen.as_ref().expect("screen is present").borrow_mut().raw = false;
        }
        disable?;
        result
    }

    fn menu_items(screen: &Screen) -> Vec<MenuItem> {
        if screen.approval || screen.menu_dismissed || !screen.input.starts_with('/') {
            return Vec::new();
        }
        let query = screen.input.to_ascii_lowercase();
        if query.starts_with("/model ") || query.starts_with("/project ") || query.starts_with("/resume ") {
            let group = if query.starts_with("/model ") { "/model " }
                else if query.starts_with("/project ") { "/project " }
                else { "/resume " };
            let mut items = Vec::new();
            items.extend(screen.menu_choices.iter()
                .filter(|choice| choice.command.to_ascii_lowercase().starts_with(group))
                .map(|choice| MenuItem {
                    command: choice.command.clone(),
                    description: choice.description.clone(),
                    submit: true,
                }));
            if group == "/project " {
                items.push(MenuItem {
                    command: "/project add ".into(),
                    description: "Connect an authorized Amity project".into(),
                    submit: false,
                });
            }
            items.retain(|item| {
                let command = item.command.to_ascii_lowercase();
                let matches = command.starts_with(&query)
                    || (group == "/project " && command.starts_with("/project use ")
                        && command["/project use ".len()..]
                            .starts_with(query.strip_prefix(group).unwrap_or_default()));
                matches && (item.submit || item.command != screen.input)
            });
            return items;
        }
        if query.starts_with("/config ") {
            let item = MenuItem {
                command: "/config host ".into(),
                description: "Set the Amity host executable path".into(),
                submit: false,
            };
            return if item.command.starts_with(&query) && item.command != screen.input {
                vec![item]
            } else { Vec::new() };
        }

        // The root menu contains only operations the CLI actually handles.
        let root: &[(&str, &str, bool, bool)] = &[
            ("/model", "Choose a configured model", false, true),
            ("/project", "Choose or connect a project", false, false),
            ("/resume", "Open a saved conversation", false, false),
            ("/session new", "Start a new conversation", true, true),
            ("/status", "Inspect the active Amity task", true, false),
            ("/review", "Review a pending proposal", true, true),
            ("/approve", "Approve a pending operation", true, true),
            ("/continue", "Continue active task work", true, true),
            ("/retry", "Retry the active task", true, true),
            ("/interrupt", "Interrupt active task work", true, true),
            ("/cancel", "Cancel the active task", true, true),
            ("/raw", "Inspect raw host details", true, true),
            ("/tools", "Show available Amity tools", true, true),
            ("/report", "Show engineering report", true, true),
            ("/reconcile", "Reconcile task evidence", true, true),
            ("/sessions", "List saved conversations", true, false),
            ("/projects", "List registered projects", true, false),
            ("/models", "List model profiles", true, false),
            ("/config", "Show Echo configuration", true, false),
            ("/task", "Start an Amity coding task", false, true),
            ("/task-steps", "Run a structured task plan", false, true),
            ("/help", "Show Echo commands", true, false),
            ("/exit", "Return to the terminal", true, false),
        ];
        root.iter().filter_map(|&(command, description, submit, requires_project)| {
            if (requires_project && !screen.project_connected) || !command.starts_with(&query) {
                return None;
            }
            Some(MenuItem {
                command: if submit { command.into() } else { format!("{command} ") },
                description: description.into(),
                submit,
            })
        }).collect()
    }

    fn screen_input_loop(&mut self, record_input: bool,
        mut session_choices: Option<&mut dyn FnMut() -> Result<Vec<MenuChoice>>>) -> Result<Input> {
        self.draw()?;
        loop {
            let event = event::read().context("reading Echo keyboard input")?;
            match event {
                Event::Resize(_, _) => { self.draw()?; }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let mut submitted = None;
                    let mut finished = None;
                    let mut load_sessions = false;
                    {
                        let mut screen = self.screen.as_ref().expect("screen is present").borrow_mut();
                        let before_input = screen.input.clone();
                        let menu = if terminal::size().is_ok_and(|(width, height)| width >= 5 && height > 8) {
                            Self::menu_items(&screen)
                        } else { Vec::new() };
                        let selected = menu.get(screen.menu_index.min(menu.len().saturating_sub(1))).cloned();
                        let mut navigated_history = false;
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                screen.input.clear(); screen.cursor = 0;
                                finished = Some(Input::Interrupt);
                            }
                            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) && screen.input.is_empty() => {
                                finished = Some(Input::Eof);
                            }
                            KeyCode::Enter => {
                                let line = match selected {
                                    Some(item) if !item.submit => {
                                        screen.input = item.command;
                                        screen.cursor = screen.input.chars().count();
                                        None
                                    }
                                    Some(item) => Some(item.command),
                                    None => Some(std::mem::take(&mut screen.input)),
                                };
                                if let Some(line) = line {
                                    screen.input.clear();
                                    screen.cursor = 0;
                                    screen.history_position = None;
                                    screen.saved_draft.clear();
                                    if record_input && !line.trim().is_empty() {
                                        screen.history.push(line.clone());
                                        if screen.history.len() > 100 { screen.history.remove(0); }
                                        submitted = Some(line.clone());
                                    }
                                    finished = Some(Input::Line(line));
                                }
                            }
                            KeyCode::Tab => {
                                if let Some(item) = selected {
                                    screen.input = item.command;
                                    screen.cursor = screen.input.chars().count();
                                }
                            }
                            KeyCode::BackTab if !menu.is_empty() => {
                                screen.menu_index = if screen.menu_index == 0 {
                                    menu.len() - 1
                                } else { screen.menu_index - 1 };
                            }
                            KeyCode::Backspace if screen.cursor > 0 => {
                                let end = byte_index(&screen.input, screen.cursor);
                                let start = byte_index(&screen.input, screen.cursor - 1);
                                screen.input.replace_range(start..end, "");
                                screen.cursor -= 1;
                            }
                            KeyCode::Delete if screen.cursor < screen.input.chars().count() => {
                                let start = byte_index(&screen.input, screen.cursor);
                                let end = byte_index(&screen.input, screen.cursor + 1);
                                screen.input.replace_range(start..end, "");
                            }
                            KeyCode::Left => screen.cursor = screen.cursor.saturating_sub(1),
                            KeyCode::Right => screen.cursor = (screen.cursor + 1).min(screen.input.chars().count()),
                            KeyCode::Home => screen.cursor = 0,
                            KeyCode::End => screen.cursor = screen.input.chars().count(),
                            KeyCode::PageUp => screen.scroll = screen.scroll.saturating_add(8),
                            KeyCode::PageDown => screen.scroll = screen.scroll.saturating_sub(8),
                            KeyCode::Up if !menu.is_empty() => {
                                screen.menu_index = if screen.menu_index == 0 {
                                    menu.len() - 1
                                } else { screen.menu_index - 1 };
                            }
                            KeyCode::Down if !menu.is_empty() => {
                                screen.menu_index = (screen.menu_index + 1) % menu.len();
                            }
                            KeyCode::Up if !screen.history.is_empty() => {
                                navigated_history = true;
                                if screen.history_position.is_none() { screen.saved_draft = screen.input.clone(); }
                                let position = screen.history_position.unwrap_or(screen.history.len()).saturating_sub(1);
                                screen.history_position = Some(position);
                                screen.input = screen.history[position].clone();
                                screen.cursor = screen.input.chars().count();
                            }
                            KeyCode::Down => {
                                if let Some(position) = screen.history_position {
                                    navigated_history = true;
                                    if position + 1 < screen.history.len() {
                                        screen.history_position = Some(position + 1);
                                        screen.input = screen.history[position + 1].clone();
                                    } else {
                                        screen.history_position = None;
                                        screen.input = screen.saved_draft.clone();
                                    }
                                    screen.cursor = screen.input.chars().count();
                                }
                            }
                            KeyCode::Esc if !menu.is_empty() => { screen.menu_dismissed = true; }
                            KeyCode::Esc => { screen.input.clear(); screen.cursor = 0; screen.history_position = None; }
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                screen.input.clear(); screen.cursor = 0;
                            }
                            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) && screen.input.len() < 16 * 1024 => {
                                let index = byte_index(&screen.input, screen.cursor);
                                screen.input.insert(index, ch);
                                screen.cursor += 1;
                            }
                            _ => {}
                        }
                        if screen.input != before_input {
                            screen.menu_index = 0;
                            screen.menu_dismissed = navigated_history;
                        }
                        if !screen.session_menu_loaded && !screen.approval
                            && screen.input.starts_with("/resume") && session_choices.is_some() {
                            screen.session_menu_loaded = true;
                            load_sessions = true;
                        }
                    }
                    if load_sessions {
                        if let Some(loader) = session_choices.as_deref_mut() {
                            match loader() {
                                Ok(choices) => self.screen.as_ref().expect("screen is present")
                                    .borrow_mut().menu_choices.extend(choices),
                                Err(error) => self.warning(&format!("Saved session choices unavailable: {error:#}")),
                            }
                        }
                    }
                    if let Some(line) = submitted { self.log(&format!("› {line}"), Tone::User); }
                    if let Some(input) = finished { return Ok(input); }
                    self.draw()?;
                }
                _ => {}
            }
        }
    }

    fn log(&self, message: &str, tone: Tone) {
        if let Some(screen) = &self.screen {
            let mut screen = screen.borrow_mut();
            let safe = Self::clean(&crate::state::redact_credentials(message));
            if matches!(tone, Tone::Operation) { screen.status = safe.clone(); }
            if matches!(tone, Tone::Assistant) { screen.status = "Ready".into(); }
            if matches!(tone, Tone::User | Tone::Assistant | Tone::Operation | Tone::Tool | Tone::Code) {
                screen.engaged = true;
            }
            Self::append_screen_lines(&mut screen, &safe, tone);
            if screen.lines.len() > 4000 {
                let excess = screen.lines.len() - 4000;
                screen.lines.drain(..excess);
            }
            screen.scroll = 0;
        }
        let _ = self.draw();
    }

    fn append_screen_lines(screen: &mut Screen, message: &str, tone: Tone) {
        let safe = Self::clean(&crate::state::redact_credentials(message));
        let mut in_code = false;
        for line in safe.split('\n') {
            let line_tone = if matches!(tone, Tone::Assistant) {
                let trimmed = line.trim_start();
                if trimmed.starts_with("```") {
                    in_code = !in_code;
                    Tone::Muted
                } else if in_code {
                    Tone::Code
                } else if trimmed.starts_with("# ") || trimmed.starts_with("## ") || trimmed.starts_with("### ") {
                    Tone::Brand
                } else {
                    Tone::Assistant
                }
            } else {
                tone
            };
            screen.lines.push(ScreenLine { text: line.to_owned(), tone: line_tone });
            if screen.lines.len() > 5000 {
                screen.lines.drain(..1000);
            }
        }
    }

    fn draw(&self) -> Result<()> {
        let Some(screen) = &self.screen else { return Ok(()); };
        let screen = screen.borrow();
        let (width, height) = terminal::size().context("reading terminal size")?;
        let width = usize::from(width);
        let height = usize::from(height);
        if width == 0 || height == 0 { return Ok(()); }
        let mut out = io::stdout();
        queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
        if height < 8 {
            self.at(&mut out, 0, 0, width, " AMITY ECHO", Tone::Brand)?;
            if height > 1 {
                self.at(&mut out, 0, 1, width, " Enlarge this terminal window to continue.", Tone::Warning)?;
            }
            out.flush()?;
            return Ok(());
        }
        if width < 5 {
            self.at(&mut out, 0, 0, width, "Echo", Tone::Brand)?;
            self.at(&mut out, 0, (height - 2) as u16, width.saturating_sub(1), &screen.input, Tone::Assistant)?;
            queue!(out, MoveTo(0, (height - 2) as u16), Show)?;
            out.flush()?;
            return Ok(());
        }
        let brand = format!("  ◇  AMITY ECHO  v{}", env!("CARGO_PKG_VERSION"));
        self.at(&mut out, 0, 0, width, &brand, Tone::Brand)?;
        let right = format!("{}  ·  {}", screen.project, screen.model);
        let right_width = right.chars().count();
        if right_width + brand.chars().count() + 4 < width {
            self.at(&mut out, (width - right_width - 2) as u16, 0, right_width,
                &right, Tone::Info)?;
        }
        self.at(&mut out, 2, 1, width.saturating_sub(4),
            &screen.cwd.display().to_string(), Tone::Muted)?;
        self.at(&mut out, 2, 2, width.saturating_sub(4),
            &format!("Session {}    Project {}    Model {}", screen.session, screen.project, screen.model), Tone::Info)?;
        self.at(&mut out, 0, 3, width.saturating_sub(1), &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;

        let footer = height - 4;
        let content_top = 4usize;
        let content_height = footer.saturating_sub(content_top);
        let content_width = width.saturating_sub(4).max(1);
        let mut rows: Vec<(String, Tone)> = Vec::new();
        for line in &screen.lines {
            for part in wrap(&line.text, content_width) {
                rows.push((part, line.tone));
            }
        }
        if !screen.engaged {
            let logo = [
                ("◇", Tone::Brand),
                ("A M I T Y   E C H O", Tone::Brand),
                ("First-party engineering agent", Tone::Muted),
                ("Describe a task or ask about your workspace", Tone::Info),
                ("Type / for commands   ·   Ctrl+C to exit", Tone::Muted),
            ];
            let start = content_top + content_height.saturating_sub(logo.len()) / 2;
            for (offset, (text, tone)) in logo.iter().enumerate() {
                if start + offset >= footer { break; }
                let length = text.chars().count();
                let x = width.saturating_sub(length) / 2;
                self.at(&mut out, x as u16, (start + offset) as u16,
                    width.saturating_sub(x), text, *tone)?;
            }
        }
        if !rows.is_empty() {
            let max_scroll = rows.len().saturating_sub(content_height);
            let scroll = screen.scroll.min(max_scroll);
            let start = rows.len().saturating_sub(content_height + scroll);
            let end = (start + content_height).min(rows.len());
            for (offset, (text, tone)) in rows[start..end].iter().enumerate() {
                self.at(&mut out, 2, (content_top + offset) as u16,
                    content_width, text, *tone)?;
            }
        }

        let menu = Self::menu_items(&screen);
        let menu_height = menu.len().min(content_height.min(8));
        if menu_height > 0 {
            let selected = screen.menu_index.min(menu.len() - 1);
            let first = selected.saturating_sub(menu_height - 1).min(menu.len() - menu_height);
            let row_width = width.saturating_sub(2);
            for (offset, item) in menu[first..first + menu_height].iter().enumerate() {
                let y = (footer - menu_height + offset) as u16;
                let active = first + offset == selected;
                let label_width = if row_width >= 45 { (row_width / 2).min(42) }
                    else { row_width.saturating_sub(2) };
                let label = truncate(&Self::clean(&item.command), label_width);
                let description = if row_width >= 45 {
                    truncate(&Self::clean(&item.description), row_width.saturating_sub(label_width + 4))
                } else { String::new() };
                let line = if description.is_empty() {
                    format!("{} {label}", if active { "›" } else { " " })
                } else {
                    format!("{} {label:<label_width$} {description}", if active { "›" } else { " " })
                };
                let line = truncate(&line, row_width);
                queue!(out, MoveTo(0, y), Clear(ClearType::CurrentLine), MoveTo(1, y))?;
                if self.color {
                    if active {
                        queue!(out, SetBackgroundColor(Color::DarkCyan), SetForegroundColor(Color::White))?;
                        queue!(out, Print(&line), Print(" ".repeat(row_width.saturating_sub(line.chars().count()))))?;
                    } else {
                        queue!(out, SetForegroundColor(Color::DarkGrey), Print(&line))?;
                    }
                    queue!(out, ResetColor)?;
                } else {
                    queue!(out, Print(&line))?;
                }
            }
        }

        self.at(&mut out, 0, footer as u16, width.saturating_sub(1), &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        let prefix = if screen.approval { " Approve operation? [y/N]  " } else { " › " };
        let prefix_width = prefix.chars().count().min(width.saturating_sub(1));
        let input_width = width.saturating_sub(prefix_width + 1);
        self.at(&mut out, 0, (footer + 1) as u16, width, prefix, Tone::Brand)?;
        let first_visible = screen.cursor.saturating_sub(input_width.saturating_sub(1));
        let visible: String = screen.input.chars().skip(first_visible).take(input_width).collect();
        self.at(&mut out, prefix_width as u16, (footer + 1) as u16,
            input_width, &visible, Tone::Assistant)?;
        self.at(&mut out, 0, (footer + 2) as u16, width.saturating_sub(1), &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        let hint = if screen.approval { " Enter confirm · Ctrl+C decline" }
            else if menu_height > 0 { " ↑↓ choose · Enter select · Tab complete · Esc close" }
            else if width >= 100 { " Enter send  ·  Ctrl+C exit  ·  ↑↓ history  ·  PgUp/PgDn scroll" }
            else { " Enter send · Ctrl+C exit · ↑↓ history" };
        let status_width = (width / 3).min(30);
        let hint_width = width.saturating_sub(status_width + 3);
        self.at(&mut out, 1, (footer + 3) as u16, hint_width, hint, Tone::Muted)?;
        let status = truncate(&screen.status, status_width);
        if !status.is_empty() {
            self.at(&mut out, (width - status.chars().count() - 2) as u16,
                (footer + 3) as u16, status.chars().count(), &status, Tone::Info)?;
        }
        let cursor_x = (prefix_width + screen.cursor.saturating_sub(first_visible)).min(width - 1) as u16;
        queue!(out, MoveTo(cursor_x, (footer + 1) as u16), Show)?;
        out.flush().context("drawing Echo terminal screen")
    }

    fn at(&self, out: &mut impl Write, x: u16, y: u16, width: usize,
        text: &str, tone: Tone) -> io::Result<()> {
        let text = truncate(text, width);
        let color = if self.color { tone.color() } else { Color::White };
        queue!(out, MoveTo(x, y), SetForegroundColor(color), Print(text), ResetColor)
    }

    fn read_with_prompt(&mut self, prompt: &str, record_history: bool) -> Result<Input> {
        if self.editor.is_none() {
            let mut editor = DefaultEditor::new().context("initializing terminal line editor")?;
            if self.history_path.exists() {
                editor.load_history(&self.history_path)
                    .with_context(|| format!("loading terminal history {}", self.history_path.display()))?;
            }
            self.editor = Some(editor);
        }
        let editor = self.editor.as_mut().expect("line editor was initialized");
        match editor.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                // Conversations are stored by the session layer after redaction.
                // Terminal history keeps only navigation commands, never arbitrary
                // instructions or approval answers that may contain credentials.
                if record_history && Self::safe_history_command(trimmed) {
                    editor.add_history_entry(trimmed)?;
                    editor
                        .save_history(&self.history_path)
                        .with_context(|| format!("saving terminal history {}", self.history_path.display()))?;
                }
                Ok(Input::Line(line))
            }
            Err(ReadlineError::Interrupted) => Ok(Input::Interrupt),
            Err(ReadlineError::Eof) => Ok(Input::Eof),
            Err(error) => Err(error).context("reading terminal input"),
        }
    }

    fn safe_history_command(line: &str) -> bool {
        let mut parts = line.split_whitespace();
        let Some(command) = parts.next() else {
            return false;
        };
        let arguments: Vec<_> = parts.collect();
        match command {
            "/help" | "/status" | "/sessions" | "/projects" | "/models" | "/config"
            | "/exit" | "/quit" | "/review" | "/raw" => arguments.is_empty(),
            "/resume" | "/project" | "/model" | "/task" => {
                arguments.len() <= 1
                    && arguments.iter().all(|part| {
                        part.len() <= 80
                            && part
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || "_-./\\".contains(ch))
                    })
            }
            _ => false,
        }
    }

    pub fn banner(&self, project: &str, session: &str, model: &str) {
        if let Some(screen) = &self.screen {
            {
                let mut screen = screen.borrow_mut();
                screen.project = project.to_owned();
                screen.session = session.to_owned();
                screen.model = model.to_owned();
            }
            let _ = self.draw();
            return;
        }
        println!("{}", self.paint("  Amity Echo", "1;36"));
        println!(
            "  {}  {}  {}",
            self.paint(&format!("Project: {project}"), "36"),
            self.paint(&format!("Session: {session}"), "2"),
            self.paint(&format!("Model: {model}"), "2")
        );
        println!("  Type /help for commands. Ctrl+C or Ctrl+D exits.\n");
    }

    pub fn context(&self, project: &str, session: &str, model: &str) {
        if let Some(screen) = &self.screen {
            {
                let mut screen = screen.borrow_mut();
                screen.project = project.to_owned();
                screen.session = session.to_owned();
                screen.model = model.to_owned();
            }
            let _ = self.draw();
            return;
        }
        println!(
            "{} {} · {} · {}",
            self.paint("Echo", "1;36"),
            Self::clean(project),
            Self::clean(session),
            Self::clean(model)
        );
    }

    pub fn assistant(&self, message: &str) {
        if self.screen.is_some() {
            self.log("ECHO", Tone::Brand);
            self.log(message, Tone::Assistant);
            self.log("", Tone::Muted);
            return;
        }
        println!("\n{}", self.paint("Echo", "1;36"));
        self.render_markdown(&Self::clean(&crate::state::redact_credentials(message)));
        println!();
    }

    pub fn info(&self, message: &str) {
        if self.screen.is_some() { self.log(message, Tone::Info); return; }
        println!("{} {}", self.paint("•", "36"), Self::clean(&crate::state::redact_credentials(message)));
    }

    pub fn warning(&self, message: &str) {
        if self.screen.is_some() { self.log(message, Tone::Warning); return; }
        eprintln!("{} {}", self.paint("!", "1;33"), Self::clean(&crate::state::redact_credentials(message)));
    }

    pub fn error(&self, message: &str) {
        if self.screen.is_some() { self.log(message, Tone::Error); return; }
        eprintln!("{} {}", self.paint("Error:", "1;31"), Self::clean(&crate::state::redact_credentials(message)));
    }

    pub fn operation(&self, message: &str) {
        if self.screen.is_some() { self.log(message, Tone::Operation); return; }
        println!("{} {}", self.paint("↳", "1;34"), Self::clean(&crate::state::redact_credentials(message)));
        let _ = io::stdout().flush();
    }

    pub fn tool_result(&self, name: &str, result: &str, success: bool) {
        if self.screen.is_some() {
            self.log(&format!("{} {name}", if success { "✓" } else { "×" }),
                if success { Tone::Tool } else { Tone::Error });
            self.log(result, Tone::Muted);
            return;
        }
        let icon = if success { "✓" } else { "×" };
        let style = if success { "32" } else { "31" };
        println!("{} {}", self.paint(icon, style), self.paint(name, "1"));
        let safe = crate::state::redact_credentials(result);
        for line in safe.lines() {
            println!("  {}", self.paint(&Self::clean(line), "2"));
        }
    }

    pub fn proposal(&self, title: &str, details: &str) {
        if self.screen.is_some() {
            self.log(&format!("PROPOSAL · {title}"), Tone::Warning);
            self.log(details, Tone::Code);
            return;
        }
        println!("\n{} {}", self.paint("Proposal:", "1;33"), Self::clean(title));
        self.render_markdown(&Self::clean(&crate::state::redact_credentials(details)));
        println!();
    }

    pub fn raw(&self, value: &str) {
        if self.screen.is_some() { self.log(value, Tone::Muted); return; }
        println!("{}", Self::clean(&crate::state::redact_credentials(value)));
    }

    fn render_markdown(&self, message: &str) {
        let mut code_language: Option<String> = None;
        for line in message.lines() {
            let trimmed = line.trim_start();
            if let Some(language) = trimmed.strip_prefix("```") {
                if code_language.is_some() {
                    code_language = None;
                } else {
                    code_language = Some(language.trim().to_ascii_lowercase());
                }
                continue;
            }
            if let Some(language) = code_language.as_deref() {
                let highlighted = self.highlight_code(line, language);
                println!("  {highlighted}");
            } else if let Some(heading) = trimmed.strip_prefix("# ") {
                println!("{}", self.paint(heading, "1;36"));
            } else if let Some(heading) = trimmed.strip_prefix("## ") {
                println!("{}", self.paint(heading, "1;36"));
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                println!("  {}", self.paint(trimmed, "0"));
            } else {
                println!("{line}");
            }
        }
    }

    fn highlight_code(&self, line: &str, language: &str) -> String {
        if !self.color {
            return line.to_owned();
        }
        if language == "json" || language == "jsonc" {
            return self.highlight_json(line);
        }
        if !matches!(
            language,
            "rust" | "rs" | "powershell" | "ps1" | "sh" | "bash" | "python" | "py" | "javascript" | "js" | "typescript" | "ts"
        ) {
            return self.paint(line, "2");
        }
        let mut output = String::new();
        let mut token = String::new();
        for ch in line.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                token.push(ch);
            } else {
                self.flush_token(&mut token, &mut output);
                output.push(ch);
            }
        }
        self.flush_token(&mut token, &mut output);
        output
    }

    fn flush_token(&self, token: &mut String, output: &mut String) {
        if token.is_empty() {
            return;
        }
        let style = if matches!(
            token.as_str(),
            "fn" | "let" | "mut" | "pub" | "struct" | "enum" | "impl" | "use" | "mod" | "match"
                | "if" | "else" | "return" | "async" | "await" | "const" | "class" | "def"
                | "function" | "import" | "from" | "export" | "try" | "catch" | "throw" | "new"
        ) {
            "1;35"
        } else if token.chars().all(|character| character.is_ascii_digit()) {
            "33"
        } else {
            "0"
        };
        output.push_str(&self.paint(token, style));
        token.clear();
    }

    fn highlight_json(&self, line: &str) -> String {
        let mut output = String::new();
        let mut string = String::new();
        let mut inside_string = false;
        let mut escaped = false;
        for ch in line.chars() {
            if inside_string {
                string.push(ch);
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    inside_string = false;
                    output.push_str(&self.paint(&string, "32"));
                    string.clear();
                }
            } else if ch == '"' {
                inside_string = true;
                string.push(ch);
            } else {
                output.push(ch);
            }
        }
        output.push_str(&string);
        output
    }

    fn paint(&self, text: &str, style: &str) -> String {
        let text = Self::clean(text);
        if self.color && style != "0" {
            format!("\x1b[{style}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    fn clean(text: &str) -> String {
        text.chars()
            .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
            .collect()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.screen.take().is_none() { return; }
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), ResetColor, Show, LeaveAlternateScreen);
    }
}

impl Tone {
    fn color(self) -> Color {
        match self {
            Self::Brand => Color::Cyan,
            Self::Muted => Color::DarkGrey,
            Self::User => Color::White,
            Self::Assistant => Color::White,
            Self::Info => Color::Blue,
            Self::Warning => Color::Yellow,
            Self::Error => Color::Red,
            Self::Operation => Color::Magenta,
            Self::Tool => Color::Green,
            Self::Code => Color::DarkCyan,
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() { return vec![String::new()]; }
    chars.chunks(width).map(|part| part.iter().collect()).collect()
}

fn byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices().nth(character_index).map(|(index, _)| index).unwrap_or(text.len())
}
