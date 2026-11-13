//! Human-facing terminal presentation. The agent and host modules own decisions;
//! this module only collects input and renders their state.

use std::env;
use std::cell::RefCell;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::{execute, queue};
use crossterm::cursor::{Hide, MoveTo, RestorePosition, SavePosition, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::events::{AgentEvent, AgentEventSink};
use crate::state::{MessageRole, Session, SessionStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Line(String),
    Interrupt,
    Eof,
}

/// A human-facing option shown in a terminal picker.
#[derive(Clone, Debug)]
pub struct MenuChoice {
    pub command: String,
    pub description: String,
}

pub struct SessionPickerEntry {
    pub id: String,
    pub title: String,
    pub project_name: String,
    pub directory: PathBuf,
    pub cwd_match: bool,
    pub created_at_ms: u128,
    pub updated_at_ms: u128,
    pub status: SessionStatus,
}

// Monochrome terminal render sampled from the supplied Echo logo.
const ECHO_LOGO_LARGE: &[&str] = &[
    "         :--::..            ...:::.",
    "     .-+*+-:                    .:-==-.",
    "   :*%%#-   .:...           ....   :=**=:",
    " .+%%%+ .=+*=:                .:==-. -+**=",
    ".*###= =#%*: :--:.         .:-: .=##= :=+*+.",
    "=*++=.+#*+.-#%+:::        :::=##-.+#%+ :-=+-",
    "+=--.:+==::##+-*#*+=====++*#*-+##::++*:.:--=",
    "=---.:+=-::**=-*#*++====++*#*-=++:.-=+:.::--",
    "-+=-:.=**=:-*#=:::        :::=#*:.=++= :---:",
    " =*+=-.=##+: :-:.          .--: :*%#= :==+-",
    "  -*#*=..-+=-.                .-+*=. =##*-",
    "   .=**+-   .....           ..:.   =#%%+:",
    "     .-===-.                   .:=*#*=.",
    "         .:::...            .::-==:.",
];

const ECHO_LOGO_SMALL: &[&str] = &[
    "    .-=-:..          ..::-:.",
    " .=*#+-:::.           .:::-++-.",
    ":*#*-=**---.        .:-:=*=:=++:",
    "+==:=+=:*#=++=----=++=**:+*+.--=",
    "==-.=+=:+*=++=----=++=*+:=+=.:--",
    ".=++-=*+---.        .---**=:=+-.",
    "  -+*=:::..          ..:--*#*-",
    "    .:-::..          .:-=+=:",
];

// Decorative model-thinking messages. Keep this collection independent of model
// context, conversation state, and engineering progress events.
const MODEL_THINKING_MESSAGES: [&str; 21] = [
    "Finding Where the Sidewalk Ends...",
    "Knocking on the Witch's Wardrobe...",
    "Downloading Data's Data...",
    "Burning Off the Part of the Lost City...",
    "Accidentally Shrinking My Children...",
    "Checking to See if That's Real Tomato Ketchup...",
    "Weighing Your Heart Against the Feather of Ma'at...",
    "Asking Treebeard Whose Side He's On...",
    "Checking to See if 2+2=4...",
    "Wondering Why My Human Failed Me...",
    "Wondering Why I've Got Red on Me...",
    "Considering a New Job...",
    "Considering 'What IS Actually My Favorite Scary Movie'...",
    "Pondering My Feet...",
    "Looking for Fabrizio...",
    "Chasing the Golden Snitch...",
    "Redrawing the Sistine Chapel...",
    "Asking George About the Rabbits...",
    "Stealing the Frog's Tea...",
    "Painting My Happy Trees...",
    "Checking If It's a Beautiful Day in the Neighborhood...",
];

const MODEL_THINKING_FRAMES: [char; 4] = ['|', '/', '-', '\\'];
const MODEL_THINKING_FRAME_INTERVAL: Duration = Duration::from_millis(125);
const MODEL_THINKING_MESSAGE_INTERVAL: Duration = Duration::from_secs(5);

pub struct Terminal {
    editor: Option<DefaultEditor>,
    history_path: PathBuf,
    color: bool,
    screen: Option<RefCell<Screen>>,
    update_check: Option<Receiver<Result<Option<semver::Version>>>>,
    plain_stream_chars: usize,
}

#[derive(Clone, Copy)]
enum Tone { Brand, Muted, User, Assistant, Info, Warning, Error, Operation, Tool, Code }

struct ScreenLine { text: String, tone: Tone, temporary: bool }

struct CommandDisplay {
    engaged: bool,
    status: String,
    scroll: usize,
    session_status: Option<SessionStatus>,
    interacted: bool,
}

#[derive(Clone)]
struct MenuItem {
    command: String,
    description: String,
}

struct Screen {
    cwd: PathBuf,
    project: String,
    session: String,
    model: String,
    lines: Vec<ScreenLine>,
    stream_start: Option<usize>,
    stream_buffer: String,
    input: String,
    cursor: usize,
    scroll: usize,
    status: String,
    model_thinking: ModelThinking,
    approval: bool,
    decision_prompt: Option<String>,
    raw: bool,
    history: Vec<String>,
    history_position: Option<usize>,
    saved_draft: String,
    engaged: bool,
    menu_index: usize,
    menu_dismissed: bool,
    update_version: Option<String>,
    text_prompt: bool,
    command: Option<CommandDisplay>,
}

struct ModelThinking {
    active: bool,
    deck: [usize; MODEL_THINKING_MESSAGES.len()],
    next_card: usize,
    current_card: Option<usize>,
    frame: usize,
    frame_at: Instant,
    message_at: Instant,
    random_state: u64,
}

impl ModelThinking {
    fn new() -> Self {
        let now = Instant::now();
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64
            ^ u64::from(std::process::id());
        let mut thinking = Self {
            active: false,
            deck: std::array::from_fn(|index| index),
            next_card: MODEL_THINKING_MESSAGES.len(),
            current_card: None,
            frame: 0,
            frame_at: now,
            message_at: now,
            random_state: if seed == 0 { 0x9e37_79b9_7f4a_7c15 } else { seed },
        };
        thinking.shuffle_deck();
        thinking
    }

    fn random(&mut self) -> u64 {
        self.random_state ^= self.random_state << 13;
        self.random_state ^= self.random_state >> 7;
        self.random_state ^= self.random_state << 17;
        self.random_state
    }

    fn shuffle_deck(&mut self) {
        for index in (1..self.deck.len()).rev() {
            let other = (self.random() as usize) % (index + 1);
            self.deck.swap(index, other);
        }
        if self.current_card == Some(self.deck[0]) {
            self.deck.swap(0, 1);
        }
        self.next_card = 0;
    }

    fn next_message(&mut self) {
        if self.next_card == self.deck.len() {
            self.shuffle_deck();
        }
        self.current_card = Some(self.deck[self.next_card]);
        self.next_card += 1;
    }

    fn begin(&mut self) {
        if self.active { return; }
        self.active = true;
        self.frame = 0;
        self.frame_at = Instant::now();
        self.message_at = self.frame_at;
        self.next_message();
    }

    fn end(&mut self) {
        self.active = false;
    }

    fn tick(&mut self) -> bool {
        if !self.active { return false; }
        let now = Instant::now();
        let mut changed = false;
        if now.duration_since(self.message_at) >= MODEL_THINKING_MESSAGE_INTERVAL {
            self.next_message();
            self.message_at = now;
            changed = true;
        }
        if now.duration_since(self.frame_at) >= MODEL_THINKING_FRAME_INTERVAL {
            self.frame = (self.frame + 1) % MODEL_THINKING_FRAMES.len();
            self.frame_at = now;
            changed = true;
        }
        changed
    }

    fn label(&self) -> Option<String> {
        if !self.active { return None; }
        let message = MODEL_THINKING_MESSAGES[*self.current_card.as_ref()?];
        Some(format!("{} {message}", MODEL_THINKING_FRAMES[self.frame]))
    }
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
            update_check: None,
            plain_stream_chars: 0,
        })
    }

    pub fn set_color(&mut self, color: bool) {
        self.color = color
            && io::stdout().is_terminal()
            && env::var_os("NO_COLOR").is_none()
            && env::var("TERM").ok().as_deref() != Some("dumb");
        let _ = self.draw();
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
            model: "not connected".into(), lines: Vec::new(),
            stream_start: None, stream_buffer: String::new(), input: String::new(),
            cursor: 0, scroll: 0, status: "Ready".into(), model_thinking: ModelThinking::new(),
            approval: false,
            decision_prompt: None, raw: false,
            history: Vec::new(), history_position: None, saved_draft: String::new(),
            engaged: false,
            menu_index: 0, menu_dismissed: false,
            update_version: None,
            text_prompt: false,
            command: None,
        }));
        let (sender, receiver) = mpsc::channel();
        self.update_check = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(crate::update::available_version());
        });
        self.draw().context("drawing Echo terminal screen")
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
            screen.stream_start = None;
            screen.stream_buffer.clear();
            screen.scroll = 0;
            screen.engaged = screen.command.is_some() || !session.messages.is_empty();
            screen.input.clear();
            screen.cursor = 0;
            screen.history_position = None;
            screen.saved_draft.clear();
            screen.approval = false;
            screen.decision_prompt = None;
            screen.text_prompt = false;
            screen.model_thinking.end();
            screen.status = session_activity(&session.status).to_owned();
            if let Some(command) = &mut screen.command {
                command.engaged = !session.messages.is_empty();
                command.status = session_activity(&session.status).to_owned();
                command.scroll = 0;
                command.session_status = Some(session.status.clone());
            }
            for message in &session.messages {
                match &message.role {
                    MessageRole::User => Self::append_conversation_lines(
                        &mut screen, &format!("› {}", message.content), Tone::User),
                    MessageRole::Assistant => {
                        Self::append_conversation_lines(&mut screen, "ECHO", Tone::Brand);
                        Self::append_conversation_lines(&mut screen, &message.content, Tone::Assistant);
                        Self::append_conversation_lines(&mut screen, "", Tone::Muted);
                    }
                    MessageRole::Tool => {
                        Self::append_conversation_lines(&mut screen, "TOOL", Tone::Tool);
                        Self::append_conversation_lines(&mut screen, &message.content, Tone::Muted);
                        Self::append_conversation_lines(&mut screen, "", Tone::Muted);
                    }
                    MessageRole::System => {
                        Self::append_conversation_lines(&mut screen, "SYSTEM", Tone::Warning);
                        Self::append_conversation_lines(&mut screen, crate::inference::user_message(&message.content), Tone::Info);
                    }
                }
            }
            if screen.command.is_none() && screen.lines.len() > 4000 {
                let excess = screen.lines.len() - 4000;
                screen.lines.drain(..excess);
            }
        }
        let _ = self.draw();
    }

    pub fn read_line(&mut self, project: &str, session: &str) -> Result<Input> {
        if self.screen.is_some() {
            {
                let mut screen = self.screen.as_ref().expect("screen is present").borrow_mut();
                screen.project = project.to_owned();
                screen.session = session.to_owned();
            }
            return self.read_screen_input(true);
        }
        let prompt = format!(
            "{} {} {} ",
            self.paint("Echo", "1;36"),
            self.paint(&format!("[{project}]"), "36"),
            self.paint(&format!("({session}) ›"), "2")
        );
        self.read_with_prompt(&prompt, true)
    }

    pub fn begin_command(&self, session: Option<&Session>) -> Result<()> {
        if let Some(screen) = &self.screen {
            let mut screen = screen.borrow_mut();
            screen.command = Some(CommandDisplay {
                engaged: screen.engaged, status: screen.status.clone(), scroll: screen.scroll,
                session_status: session.map(|s| s.status.clone()), interacted: false,
            });
            screen.engaged = true;
        }
        self.draw()
    }

    pub fn command_interacted(&self) -> bool {
        self.screen.as_ref().is_some_and(|screen| screen.borrow().command.as_ref()
            .is_some_and(|command| command.interacted))
    }

    fn mark_command_interaction(&self) {
        if let Some(screen) = &self.screen {
            if let Some(command) = &mut screen.borrow_mut().command { command.interacted = true; }
        }
    }

    /// Read-only command results remain available until their temporary view closes.
    pub fn dismiss_command_output(&mut self) -> Result<()> {
        let Some(screen) = &self.screen else { return Ok(()); };
        terminal::enable_raw_mode()?;
        screen.borrow_mut().raw = true;
        let result = (|| -> Result<()> {
            loop {
                self.draw()?;
                let (width, height) = terminal::size()?;
                if height > 0 {
                    let mut out = io::stdout();
                    queue!(out, Hide, MoveTo(0, height - 1), Clear(ClearType::CurrentLine))?;
                    self.at(&mut out, 0, height - 1, usize::from(width).saturating_sub(1),
                        " Enter / Esc back · PgUp/PgDn scroll", Tone::Muted)?;
                    out.flush()?;
                }
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                        KeyCode::Enter | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                        KeyCode::Up | KeyCode::PageUp => {
                            let mut screen = screen.borrow_mut(); screen.scroll = screen.scroll.saturating_add(8);
                        },
                        KeyCode::Down | KeyCode::PageDown => {
                            let mut screen = screen.borrow_mut(); screen.scroll = screen.scroll.saturating_sub(8);
                        },
                        _ => {},
                    },
                    _ => {},
                }
            }
        })();
        let restored = terminal::disable_raw_mode();
        if restored.is_ok() { screen.borrow_mut().raw = false; }
        restored?;
        result
    }

    pub fn end_command(&self, session: Option<&Session>) -> Result<()> {
        if let Some(screen) = &self.screen {
            let mut screen = screen.borrow_mut();
            if let Some(command) = screen.command.take() {
                // Streaming indices must follow the retained conversation lines.
                screen.stream_start = screen.stream_start.map(|start|
                    screen.lines.iter().take(start).filter(|line| !line.temporary).count());
                screen.lines.retain(|line| !line.temporary);
                screen.engaged = command.engaged;
                screen.scroll = command.scroll;
                screen.status = if session.map(|s| &s.status) != command.session_status.as_ref() {
                    session.map(|s| session_activity(&s.status)).unwrap_or("Ready").to_owned()
                } else { command.status };
                screen.model_thinking.end();
                screen.approval = false;
                screen.text_prompt = false;
                screen.decision_prompt = None;
                screen.menu_index = 0;
            }
        }
        self.draw()
    }

    /// Choose a real action without requiring the user to remember its ID.
    pub fn wait_for_work<T:Send+'static>(&mut self,work:impl FnOnce()->Result<T>+Send+'static)->Result<T> {
        let worker=std::thread::spawn(work);
        self.begin_busy()?; let mut cancelled=false;
        while !worker.is_finished() {
            match self.poll_busy() {
                Some(crate::events::BusyInput::Cancel)=>cancelled=true,
                Some(crate::events::BusyInput::Message(message))=>{
                    if let Some(screen)=&self.screen {let mut s=screen.borrow_mut();s.input=message;s.cursor=s.input.chars().count();}
                },None=>{},
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.end_busy()?;
        let result=worker.join().map_err(|_|anyhow::anyhow!("Could not connect to the model. Try again."))?;
        if cancelled {anyhow::bail!("Model selection cancelled.");} result
    }

    pub fn choose_model(&mut self, models: &[crate::inference::ModelChoice], initial: usize,
        current: Option<&crate::inference::ModelSelection>) -> Result<Option<crate::inference::ModelSelection>> {
        if self.screen.is_none(){anyhow::bail!("Open Echo's interactive terminal to choose a model.");}
        if models.is_empty(){return Ok(None);}
        let choices: Vec<_> = models.iter().map(|choice| MenuChoice {
            command: format!("{}{}", choice.model.label(),
                if current.is_some_and(|m| m.root == choice.model.root) { " (current)" } else { "" }),
            description: choice.model.description(),
        }).collect();
        terminal::enable_raw_mode()?;
        self.screen.as_ref().unwrap().borrow_mut().raw=true;
        let result=(||->Result<Option<crate::inference::ModelSelection>> {
            let mut selected=initial.min(models.len()-1);
            let mut effort: Option<(Vec<MenuChoice>, usize)> = None;
            loop {
                let title = if effort.is_some() {
                    format!("Select Reasoning Level for {}", models[selected].model.label())
                } else { "Select Model and Effort".to_owned() };
                let (rows, index) = match &effort {
                    Some((options, index)) => (options.as_slice(), *index),
                    None => (choices.as_slice(), selected),
                };
                let page = self.draw_model_picker(&title, rows, index)?;
                let count = rows.len();
                match event::read()? {
                    Event::Resize(_,_)=>{},
                    Event::Key(key) if key.kind!=KeyEventKind::Release=>match key.code {
                        KeyCode::Esc=>{if effort.is_some() {effort=None;} else {return Ok(None);}},
                        KeyCode::Char('c'|'d') if key.modifiers.contains(KeyModifiers::CONTROL)=>return Ok(None),
                        KeyCode::Enter if page > 0 => {
                            let model = &models[selected];
                            if let Some((_, index)) = &effort {
                                return Ok(Some(model.reasoning[*index].selection.clone()));
                            }
                            if !model.model.ready() || model.reasoning.is_empty() {
                                return Ok(Some(model.model.clone()));
                            }
                            let initial = model.reasoning.iter().position(|r| current == Some(&r.selection))
                                .or_else(|| model.reasoning.iter().position(|r| r.is_default)).unwrap_or(0);
                            let options = model.reasoning.iter().map(|r| MenuChoice {
                                command: format!("{}{}", r.label, if current == Some(&r.selection) {
                                    " (current)"
                                } else if r.is_default { " (default)" } else { "" }),
                                description: r.description.clone(),
                            }).collect();
                            effort = Some((options, initial));
                        },
                        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                        | KeyCode::Home | KeyCode::End => {
                            let index = match &mut effort { Some((_, i)) => i, None => &mut selected };
                            *index = match key.code {
                                KeyCode::Up => index.saturating_sub(1),
                                KeyCode::Down => (*index + 1).min(count - 1),
                                KeyCode::PageUp => index.saturating_sub(page.max(1)),
                                KeyCode::PageDown => index.saturating_add(page.max(1)).min(count - 1),
                                KeyCode::Home => 0,
                                _ => count - 1,
                            };
                        },
                        _=>{},
                    },_=>{},
                }
            }
        })();
        let restore=terminal::disable_raw_mode();
        if restore.is_ok(){self.screen.as_ref().unwrap().borrow_mut().raw=false;}
        restore?; self.draw()?;result
    }
    fn draw_model_picker(&self, title: &str, choices: &[MenuChoice], selected: usize) -> Result<usize> {
        let (width, height) = terminal::size()?;
        Ok(render_model_picker(&mut io::stdout(), width, height, title, choices, selected)?)
    }

    pub fn choose_option(&mut self, title: &str, options: &[MenuChoice]) -> Result<Option<usize>> {
        self.mark_command_interaction();
        if options.is_empty() { return Ok(None); }
        if self.screen.is_none() {
            self.info(title);
            for (index, option) in options.iter().enumerate() {
                self.info(&format!("{}. {} — {}", index + 1, option.command, option.description));
            }
            loop {
                match self.read_with_prompt("Choose a number (Enter cancels): ", false)? {
                    Input::Line(answer) if answer.trim().is_empty() => return Ok(None),
                    Input::Line(answer) => {
                        if let Ok(index) = answer.trim().parse::<usize>() {
                            if let Some(index) = index.checked_sub(1).filter(|index| *index < options.len()) {
                                return Ok(Some(index));
                            }
                        }
                        self.info(&format!("Choose a number from 1 to {}.", options.len()));
                    }
                    Input::Interrupt | Input::Eof => return Ok(None),
                }
            }
        }
        terminal::enable_raw_mode().context("enabling Echo option picker input")?;
        self.screen.as_ref().expect("screen is present").borrow_mut().raw = true;
        let result = self.option_picker_loop(title, options);
        let disable = terminal::disable_raw_mode().context("restoring normal terminal input");
        if disable.is_ok() {
            self.screen.as_ref().expect("screen is present").borrow_mut().raw = false;
        }
        let redraw = self.draw();
        disable?;
        redraw?;
        result
    }

    fn option_picker_loop(&self, title: &str, options: &[MenuChoice]) -> Result<Option<usize>> {
        let mut selected = 0usize;
        loop {
            self.draw_option_picker(title, options, selected)?;
            let list_visible = terminal::size().is_ok_and(|(width, height)| width >= 24 && height >= 8);
            match event::read().context("reading Echo option picker input")? {
                Event::Resize(_, _) => {}
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
                    KeyCode::Enter if list_visible => return Ok(Some(selected)),
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(options.len() - 1),
                    KeyCode::PageUp => selected = selected.saturating_sub(10),
                    KeyCode::PageDown => selected = (selected + 10).min(options.len() - 1),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn draw_option_picker(&self, title: &str, options: &[MenuChoice], selected: usize) -> Result<()> {
        let (width, height) = terminal::size().context("reading terminal size")?;
        let width = usize::from(width);
        let height = usize::from(height);
        if width == 0 || height == 0 { return Ok(()); }
        let mut out = io::stdout();
        queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
        if width < 24 || height < 8 {
            self.at(&mut out, 0, 0, width, " AMITY ECHO", Tone::Brand)?;
            if height > 1 { self.at(&mut out, 0, 1, width, " Enlarge the terminal to choose an option.", Tone::Warning)?; }
            out.flush()?;
            return Ok(());
        }
        self.at(&mut out, 1, 0, width.saturating_sub(2), &format!("AMITY ECHO  /  {}", Self::clean(title)), Tone::Brand)?;
        self.at(&mut out, 0, 2, width.saturating_sub(1), &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        let rows = height.saturating_sub(6).max(1);
        let first = selected.saturating_sub(rows.saturating_sub(1))
            .min(options.len().saturating_sub(rows));
        let row_width = width.saturating_sub(2);
        for (offset, option) in options[first..(first + rows).min(options.len())].iter().enumerate() {
            let active = first + offset == selected;
            let label_width = if row_width >= 45 { (row_width / 2).min(42) }
                else { row_width.saturating_sub(2) };
            let label = truncate(&Self::clean(&option.command), label_width);
            let description = if row_width >= 45 {
                truncate(&Self::clean(&option.description), row_width.saturating_sub(label_width + 4))
            } else { String::new() };
            let line = if description.is_empty() {
                format!("{} {label}", if active { "›" } else { " " })
            } else {
                format!("{} {label:<label_width$} {description}", if active { "›" } else { " " })
            };
            let line = truncate(&line, row_width);
            let y = (3 + offset) as u16;
            queue!(out, MoveTo(0, y), Clear(ClearType::CurrentLine), MoveTo(1, y))?;
            if self.color && active {
                queue!(out, SetBackgroundColor(Color::DarkCyan), SetForegroundColor(Color::White),
                    Print(&line), Print(" ".repeat(row_width.saturating_sub(line.chars().count()))), ResetColor)?;
            } else {
                if self.color { queue!(out, SetForegroundColor(Color::Grey))?; }
                queue!(out, Print(&line), ResetColor)?;
            }
        }
        self.at(&mut out, 0, (height - 2) as u16, width.saturating_sub(1),
            &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        self.at(&mut out, 1, (height - 1) as u16, width.saturating_sub(2),
            &format!("{} options · ↑↓ choose · Enter select · Esc back", options.len()), Tone::Muted)?;
        out.flush().context("drawing Echo option picker")
    }

    /// Ask for a setup value without displaying or saving the answer in terminal history.
    pub fn prompt_text(&mut self, question: &str) -> Result<Option<String>> {
        self.info(question);
        if self.screen.is_none() {
            return match self.read_with_prompt("› ", false)? {
                Input::Line(answer) if answer.trim().is_empty() => Ok(None),
                Input::Line(answer) => Ok(Some(answer.trim().to_owned())),
                Input::Interrupt | Input::Eof => Ok(None),
            };
        }
        {
            let mut screen = self.screen.as_ref().expect("screen is present").borrow_mut();
            screen.approval = true;
            screen.text_prompt = true;
            screen.decision_prompt = Some(" Answer  ".into());
        }
        let result = self.read_screen_input(false);
        {
            let mut screen = self.screen.as_ref().expect("screen is present").borrow_mut();
            screen.approval = false;
            screen.text_prompt = false;
            screen.decision_prompt = None;
            screen.input.clear();
            screen.cursor = 0;
        }
        let redraw = self.draw();
        let answer = result?;
        redraw?;
        match answer {
            Input::Line(answer) if answer.trim().is_empty() => Ok(None),
            Input::Line(answer) => Ok(Some(answer.trim().to_owned())),
            Input::Interrupt | Input::Eof => Ok(None),
        }
    }

    /// Browse saved conversations without changing the active session until one is chosen.
    pub fn choose_session(&mut self, entries: &[SessionPickerEntry]) -> Result<Option<String>> {
        if self.screen.is_none() {
            anyhow::bail!("opening the conversation browser requires an interactive terminal");
        }
        terminal::enable_raw_mode().context("enabling Echo session browser input")?;
        self.screen.as_ref().expect("screen is present").borrow_mut().raw = true;
        let result = self.session_picker_loop(entries);
        let disable = terminal::disable_raw_mode().context("restoring normal terminal input");
        if disable.is_ok() {
            self.screen.as_ref().expect("screen is present").borrow_mut().raw = false;
        }
        let redraw = self.draw();
        disable?;
        redraw?;
        result
    }

    /// Show the public command reference without changing the active conversation.
    pub fn show_help(&mut self, commands: &[crate::commands::CommandSpec]) -> Result<()> {
        if self.screen.is_none() {
            let width = terminal::size().ok().map(|(width, _)| usize::from(width))
                .filter(|width| *width > 0).unwrap_or(80);
            println!("{}", self.paint("ECHO HELP", "1;36"));
            println!("{}", "─".repeat(width.saturating_sub(4).min(52).max(1)));
            if width < 19 {
                println!("Enlarge the terminal to read help.");
                return Ok(());
            }
            for (line, _) in format_help_lines(commands, width) {
                println!("{line}");
            }
            return Ok(());
        }
        terminal::enable_raw_mode().context("enabling Echo help input")?;
        self.screen.as_ref().expect("screen is present").borrow_mut().raw = true;
        let result = self.help_screen_loop(commands);
        let disable = terminal::disable_raw_mode().context("restoring normal terminal input");
        if disable.is_ok() {
            self.screen.as_ref().expect("screen is present").borrow_mut().raw = false;
        }
        let redraw = self.draw();
        disable?;
        redraw?;
        result
    }

    fn help_screen_loop(&self, commands: &[crate::commands::CommandSpec]) -> Result<()> {
        let mut scroll = 0usize;
        loop {
            let (max_scroll, page_rows) = self.draw_help_screen(commands, scroll)?;
            scroll = scroll.min(max_scroll);
            match event::read().context("reading Echo help input")? {
                Event::Resize(_, _) => {}
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                    KeyCode::Up => scroll = scroll.saturating_sub(1),
                    KeyCode::Down => scroll = (scroll + 1).min(max_scroll),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(page_rows),
                    KeyCode::PageDown => scroll = scroll.saturating_add(page_rows).min(max_scroll),
                    KeyCode::Home => scroll = 0,
                    KeyCode::End => scroll = max_scroll,
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn draw_help_screen(&self, commands: &[crate::commands::CommandSpec],
        scroll: usize) -> Result<(usize, usize)> {
        let (width, height) = terminal::size().context("reading terminal size")?;
        let width = usize::from(width);
        let height = usize::from(height);
        if width == 0 || height == 0 { return Ok((0, 1)); }
        let mut out = io::stdout();
        queue!(out, Hide, MoveTo(0, 0), Clear(ClearType::All))?;
        if width < 19 || height < 8 {
            self.at(&mut out, 0, 0, width, " ECHO HELP", Tone::Brand)?;
            if height > 1 {
                self.at(&mut out, 0, 1, width, " Enlarge the terminal to read help.", Tone::Warning)?;
            }
            out.flush()?;
            return Ok((0, 1));
        }
        self.at(&mut out, 2, 0, width.saturating_sub(4), "ECHO HELP", Tone::Brand)?;
        self.at(&mut out, 2, 1, width.saturating_sub(4),
            &"─".repeat(width.saturating_sub(4).min(52)), Tone::Muted)?;
        let lines = format_help_lines(commands, width);
        let page_rows = height.saturating_sub(5);
        let max_scroll = lines.len().saturating_sub(page_rows);
        let start = scroll.min(max_scroll);
        for (offset, (line, tone)) in lines.iter().skip(start).take(page_rows).enumerate() {
            self.at(&mut out, 2, (3 + offset) as u16, width.saturating_sub(4), line, *tone)?;
        }
        self.at(&mut out, 0, (height - 2) as u16, width.saturating_sub(1),
            &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        let position = if max_scroll == 0 { "All commands".to_owned() }
            else { format!("Line {} of {}", start + 1, lines.len()) };
        self.at(&mut out, 2, (height - 1) as u16, width.saturating_sub(4),
            &format!("{position} · ↑↓ scroll · PgUp/PgDn page · Esc back"), Tone::Muted)?;
        out.flush().context("drawing Echo help")?;
        Ok((max_scroll, page_rows))
    }

    fn session_picker_loop(&self, entries: &[SessionPickerEntry]) -> Result<Option<String>> {
        let mut query = String::new();
        let mut cwd_only = true;
        let mut status_filter = 0u8;
        let mut sort_created = false;
        let mut selected = 0usize;
        loop {
            let visible = session_picker_rows(entries, &query, cwd_only, status_filter, sort_created);
            selected = selected.min(visible.len().saturating_sub(1));
            self.draw_session_picker(&visible, &query, cwd_only, status_filter, sort_created, selected)?;
            let list_visible = terminal::size().is_ok_and(|(width, height)| width >= 24 && height >= 8);
            match event::read().context("reading Echo session browser input")? {
                Event::Resize(_, _) => {}
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
                    KeyCode::Enter if list_visible && !visible.is_empty() => return Ok(Some(visible[selected].id.clone())),
                    KeyCode::Up if !visible.is_empty() => selected = selected.saturating_sub(1),
                    KeyCode::Down if !visible.is_empty() => selected = (selected + 1).min(visible.len() - 1),
                    KeyCode::PageUp if !visible.is_empty() => selected = selected.saturating_sub(10),
                    KeyCode::PageDown if !visible.is_empty() => selected = (selected + 10).min(visible.len() - 1),
                    KeyCode::Tab => { cwd_only = !cwd_only; selected = 0; }
                    KeyCode::F(2) => { sort_created = !sort_created; selected = 0; }
                    KeyCode::F(3) => { status_filter = (status_filter + 1) % 3; selected = 0; }
                    KeyCode::Backspace => { query.pop(); selected = 0; }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) && query.len() < 256 => {
                        query.push(ch);
                        selected = 0;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn draw_session_picker(&self, visible: &[&SessionPickerEntry], query: &str,
        cwd_only: bool, status_filter: u8, sort_created: bool, selected: usize) -> Result<()> {
        let (width, height) = terminal::size().context("reading terminal size")?;
        let width = usize::from(width);
        let height = usize::from(height);
        if width == 0 || height == 0 { return Ok(()); }
        let mut out = io::stdout();
        queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
        if height < 8 || width < 24 {
            self.at(&mut out, 0, 0, width, " AMITY ECHO  /  RESUME", Tone::Brand)?;
            if height > 1 { self.at(&mut out, 0, 1, width, " Enlarge the terminal to browse sessions.", Tone::Warning)?; }
            out.flush()?;
            return Ok(());
        }
        self.at(&mut out, 1, 0, width.saturating_sub(2), "AMITY ECHO  /  RESUME", Tone::Brand)?;
        if let Some(screen) = &self.screen {
            self.at(&mut out, 1, 1, width.saturating_sub(2),
                &format!("Directory: {}", screen.borrow().cwd.display()), Tone::Muted)?;
        }
        let scope = if cwd_only { "[CWD] All" } else { "CWD [All]" };
        let status = match status_filter {
            1 => "All [Active] Finished",
            2 => "All Active [Finished]",
            _ => "[All] Active Finished",
        };
        let sort = if sort_created { "Updated [Created]" } else { "[Updated] Created" };
        self.at(&mut out, 1, 2, width.saturating_sub(2),
            &format!("Scope: {scope}   Status: {status}   Sort: {sort}"), Tone::Info)?;
        self.at(&mut out, 1, 3, width.saturating_sub(2),
            &format!("Search: {query}"), Tone::Assistant)?;
        self.at(&mut out, 0, 4, width.saturating_sub(1),
            &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        let rows = height.saturating_sub(7);
        if visible.is_empty() {
            let message = if cwd_only {
                "No matching sessions in this directory. Press Tab to browse all."
            } else { "No matching sessions." };
            self.at(&mut out, 2, 5, width.saturating_sub(4), message, Tone::Muted)?;
        } else {
            let first = selected.saturating_sub(rows.saturating_sub(1))
                .min(visible.len().saturating_sub(rows));
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
            for (offset, entry) in visible[first..(first + rows).min(visible.len())].iter().enumerate() {
                let y = 5 + offset;
                let active = first + offset == selected;
                let age = relative_age(now.saturating_sub(entry.updated_at_ms));
                let title = if cwd_only { entry.title.clone() } else {
                    let folder = entry.directory.file_name()
                        .map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                    format!("{}  ·  {} / {}", entry.title, entry.project_name, folder)
                };
                let label = format!("{} {:>8}  {:<10}  {}", if active { "›" } else { " " },
                    age, session_status_label(&entry.status), title);
                let row_width = width.saturating_sub(2);
                let label = truncate(&Self::clean(&label), row_width);
                queue!(out, MoveTo(0, y as u16), Clear(ClearType::CurrentLine), MoveTo(1, y as u16))?;
                if self.color && active {
                    queue!(out, SetBackgroundColor(Color::DarkCyan), SetForegroundColor(Color::White),
                        Print(&label), Print(" ".repeat(row_width.saturating_sub(label.chars().count()))), ResetColor)?;
                } else {
                    if self.color { queue!(out, SetForegroundColor(if active { Color::White } else { Color::Grey }))?; }
                    queue!(out, Print(&label), ResetColor)?;
                }
            }
        }
        self.at(&mut out, 0, (height - 2) as u16, width.saturating_sub(1),
            &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        self.at(&mut out, 1, (height - 1) as u16, width.saturating_sub(2),
            &format!("{} sessions · ↑↓ choose · Enter open · Tab scope · F2 sort · F3 status · Esc back", visible.len()),
            Tone::Muted)?;
        out.flush().context("drawing Echo session browser")
    }

    pub fn ask_approval(&mut self, _description: &str) -> Result<bool> {
        self.mark_command_interaction();
        if self.screen.is_some() {
            self.screen.as_ref().expect("screen is present").borrow_mut().approval = true;
            loop {
                match self.read_screen_input(false)? {
                    Input::Line(answer) => match answer.trim().to_ascii_lowercase().as_str() {
                        "y" | "yes" => { self.screen.as_ref().expect("screen is present").borrow_mut().approval = false; return Ok(true); }
                        "" | "n" | "no" => { self.screen.as_ref().expect("screen is present").borrow_mut().approval = false; return Ok(false); }
                        _ => self.log("Enter y or n.", Tone::Info),
                    },
                    Input::Interrupt | Input::Eof => { self.screen.as_ref().expect("screen is present").borrow_mut().approval = false; return Ok(false); }
                }
            }
        }
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

    fn ask_policy_decision(&mut self, description: &str, choices: &[String]) -> Result<Option<String>> {
        self.mark_command_interaction();
        self.warning(description);
        if choices.is_empty() { return Ok(None); }
        for (index, choice) in choices.iter().enumerate() {
            self.info(&format!("{}. {choice}", index + 1));
        }
        if let Some(screen) = &self.screen {
            let mut screen = screen.borrow_mut();
            screen.approval = true;
            screen.decision_prompt = Some(format!(" Decision [1-{}] (Enter waits)  ", choices.len()));
        }
        let result = (|| -> Result<Option<String>> {
            loop {
                let input = if self.screen.is_some() {
                    self.read_screen_input(false)?
                } else {
                    let prompt = format!("{} ", self.paint(
                        &format!("Decision [1-{}] (Enter waits):", choices.len()), "1;33"));
                    self.read_with_prompt(&prompt, false)?
                };
                match input {
                    Input::Line(answer) if answer.trim().is_empty() => return Ok(None),
                    Input::Line(answer) => {
                        if let Ok(index) = answer.trim().parse::<usize>() {
                            if let Some(choice) = index.checked_sub(1).and_then(|index| choices.get(index)) {
                                return Ok(Some(choice.clone()));
                            }
                        }
                        self.info(&format!("Choose a number from 1 to {}, or press Enter to wait.", choices.len()));
                    }
                    Input::Interrupt | Input::Eof => return Ok(None),
                }
            }
        })();
        if let Some(screen) = &self.screen {
            let mut screen = screen.borrow_mut();
            screen.approval = false;
            screen.decision_prompt = None;
            screen.input.clear();
            screen.cursor = 0;
        }
        let _ = self.draw();
        result
    }

    fn read_screen_input(&mut self, record_input: bool) -> Result<Input> {
        terminal::enable_raw_mode().context("enabling Echo keyboard input")?;
        self.screen.as_ref().expect("screen is present").borrow_mut().raw = true;
        let result = self.screen_input_loop(record_input);
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
        crate::commands::COMMANDS.iter()
            .filter(|command| command.name.starts_with(&query))
            .map(|command| MenuItem {
                command: command.name.to_owned(),
                description: command.description.to_owned(),
            })
            .collect()
    }

    fn screen_input_loop(&mut self, record_input: bool) -> Result<Input> {
        self.refresh_update_notice();
        self.draw()?;
        loop {
            if self.update_check.is_some()
                && !event::poll(Duration::from_millis(250)).context("waiting for Echo keyboard input")? {
                if self.refresh_update_notice() { self.draw()?; }
                continue;
            }
            let event = event::read().context("reading Echo keyboard input")?;
            match event {
                Event::Resize(_, _) => { self.draw()?; }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let mut submitted = None;
                    let mut finished = None;
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
                                let line = selected.map(|item| item.command)
                                    .unwrap_or_else(|| std::mem::take(&mut screen.input));
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
                            KeyCode::Esc if screen.text_prompt => {
                                screen.input.clear(); screen.cursor = 0;
                                finished = Some(Input::Interrupt);
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
                    }
                    if let Some(line) = submitted {
                        if !line.trim_start().starts_with('/') { self.log(&format!("› {line}"), Tone::User); }
                    }
                    if let Some(input) = finished { return Ok(input); }
                    self.draw()?;
                }
                _ => {}
            }
        }
    }

    fn refresh_update_notice(&mut self) -> bool {
        let Some(receiver) = &self.update_check else { return false; };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.update_check = None;
                return false;
            }
        };
        self.update_check = None;
        match outcome {
            Ok(Some(version)) => {
                if let Some(screen) = &self.screen {
                    screen.borrow_mut().update_version = Some(version.to_string());
                }
                true
            }
            Ok(None) => false,
            Err(_) => false,
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
            if screen.command.is_none() && screen.lines.len() > 4000 {
                let excess = screen.lines.len() - 4000;
                screen.lines.drain(..excess);
            }
            screen.scroll = 0;
        }
        let _ = self.draw();
    }

    fn append_screen_lines(screen: &mut Screen, message: &str, tone: Tone) {
        Self::push_screen_lines(screen, message, tone, screen.command.is_some());
    }

    fn append_conversation_lines(screen: &mut Screen, message: &str, tone: Tone) {
        Self::push_screen_lines(screen, message, tone, false);
    }

    fn push_screen_lines(screen: &mut Screen, message: &str, tone: Tone, temporary: bool) {
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
            screen.lines.push(ScreenLine { text: line.to_owned(), tone: line_tone, temporary });
            if screen.command.is_none() && screen.lines.len() > 5000 {
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
        let brand = format!("  AMITY ECHO  v{}", env!("CARGO_PKG_VERSION"));
        if let Some(version) = &screen.update_version {
            let full = format!("Update v{version} available · echocli update");
            let command = "echocli update";
            let label = if brand.chars().count() + full.chars().count() + 4 < width {
                full.as_str()
            } else { command };
            if brand.chars().count() + label.chars().count() + 4 < width {
                self.at(&mut out, 0, 0, width, &brand, Tone::Brand)?;
                self.at(&mut out, (width - label.chars().count() - 2) as u16,
                    0, label.chars().count(), label, Tone::Warning)?;
            } else {
                self.at(&mut out, 0, 0, width, label, Tone::Warning)?;
            }
        } else {
            self.at(&mut out, 0, 0, width, &brand, Tone::Brand)?;
            let right = format!("{}  ·  {}", screen.project, screen.model);
            let right_width = right.chars().count();
            if right_width + brand.chars().count() + 4 < width {
                self.at(&mut out, (width - right_width - 2) as u16, 0, right_width,
                    &right, Tone::Info)?;
            }
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
            let (art, art_width): (&[&str], usize) = if width >= 48 && content_height >= 16 {
                (ECHO_LOGO_LARGE, 44)
            } else if width >= 34 && content_height >= 10 {
                (ECHO_LOGO_SMALL, 32)
            } else { (&[], 0) };
            let labels: &[(&str, Tone)] = if content_height >= art.len() + 3 {
                &[("A M I T Y   E C H O", Tone::Brand),
                    ("Describe a task or ask about your workspace", Tone::Info),
                    ("Type / for commands   ·   Ctrl+C to exit", Tone::Muted)]
            } else if content_height >= art.len() + 2 {
                &[("A M I T Y   E C H O", Tone::Brand),
                    ("Type / for commands   ·   Ctrl+C to exit", Tone::Muted)]
            } else { &[("A M I T Y   E C H O", Tone::Brand)] };
            let start = content_top + content_height.saturating_sub(art.len() + labels.len()) / 2;
            let art_x = width.saturating_sub(art_width) / 2;
            for (offset, line) in art.iter().enumerate() {
                self.at(&mut out, art_x as u16, (start + offset) as u16,
                    art_width, line, Tone::Assistant)?;
            }
            for (offset, (line, tone)) in labels.iter().enumerate() {
                let y = start + art.len() + offset;
                if y >= footer { break; }
                let x = width.saturating_sub(line.chars().count()) / 2;
                self.at(&mut out, x as u16, y as u16,
                    width.saturating_sub(x), line, *tone)?;
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
        let prefix = if screen.approval {
            screen.decision_prompt.as_deref().unwrap_or(" Approve operation? [y/N]  ")
        } else { " › " };
        let prefix_width = prefix.chars().count().min(width.saturating_sub(1));
        let input_width = width.saturating_sub(prefix_width + 1);
        self.at(&mut out, 0, (footer + 1) as u16, width, prefix, Tone::Brand)?;
        let first_visible = screen.cursor.saturating_sub(input_width.saturating_sub(1));
        let visible: String = screen.input.chars().skip(first_visible).take(input_width).collect();
        self.at(&mut out, prefix_width as u16, (footer + 1) as u16,
            input_width, &visible, Tone::Assistant)?;
        self.at(&mut out, 0, (footer + 2) as u16, width.saturating_sub(1), &"─".repeat(width.saturating_sub(1)), Tone::Muted)?;
        self.draw_footer_status(&mut out, &screen, width, height, menu_height)?;
        let cursor_x = (prefix_width + screen.cursor.saturating_sub(first_visible)).min(width - 1) as u16;
        queue!(out, MoveTo(cursor_x, (footer + 1) as u16), Show)?;
        out.flush().context("drawing Echo terminal screen")
    }

    fn draw_footer_status(&self, out: &mut impl Write, screen: &Screen,
        width: usize, height: usize, menu_height: usize) -> Result<()> {
        let thinking = screen.model_thinking.label();
        let hint = if screen.text_prompt { " Enter submit · Esc or Ctrl+C cancel" }
            else if screen.decision_prompt.is_some() { " Enter choose · Ctrl+C wait" }
            else if screen.approval { " Enter confirm · Ctrl+C decline" }
            else if thinking.is_some() && width >= 110 { " Ctrl+C cancel · type to queue" }
            else if thinking.is_some() && width >= 70 { " Ctrl+C cancel" }
            else if thinking.is_some() { "" }
            else if menu_height > 0 { " ↑↓ choose · Enter select · Tab complete · Esc close" }
            else if width >= 100 { " Enter send  ·  Ctrl+C exit  ·  ↑↓ history  ·  PgUp/PgDn scroll" }
            else { " Enter send · Ctrl+C exit · ↑↓ history" };
        let status_width = if thinking.is_some() {
            width.saturating_sub(hint.chars().count() + 3)
        } else {
            (width / 3).min(30)
        };
        let hint_width = width.saturating_sub(status_width + 3);
        self.at(out, 1, (height - 1) as u16, hint_width, hint, Tone::Muted)?;
        let status = truncate(thinking.as_deref().unwrap_or(&screen.status), status_width);
        if !status.is_empty() {
            self.at(out, (width - status.chars().count() - 2) as u16,
                (height - 1) as u16, status.chars().count(), &status, Tone::Info)?;
        }
        Ok(())
    }

    fn redraw_footer_status(&self) -> Result<()> {
        let Some(screen) = &self.screen else { return Ok(()); };
        let (width, height) = terminal::size().context("reading terminal size")?;
        let width = usize::from(width);
        let height = usize::from(height);
        if width < 5 || height < 8 { return Ok(()); }
        let screen = screen.borrow();
        let menu_height = Self::menu_items(&screen).len().min(height.saturating_sub(8).min(8));
        let mut out = io::stdout();
        queue!(out, SavePosition, MoveTo(0, (height - 1) as u16), Clear(ClearType::CurrentLine))?;
        self.draw_footer_status(&mut out, &screen, width, height, menu_height)?;
        queue!(out, RestorePosition)?;
        out.flush().context("drawing Echo model status")
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
        crate::commands::find(line).is_some()
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
            if let Some(screen) = &self.screen {
                let mut screen = screen.borrow_mut();
                if let Some(start) = screen.stream_start.take() { screen.lines.truncate(start); }
                screen.stream_buffer.clear();
            }
            let mut screen = self.screen.as_ref().unwrap().borrow_mut();
            Self::append_conversation_lines(&mut screen, "ECHO", Tone::Brand);
            Self::append_conversation_lines(&mut screen, message, Tone::Assistant);
            Self::append_conversation_lines(&mut screen, "", Tone::Muted);
            screen.engaged = true;
            screen.scroll = 0;
            screen.status = "Ready".into();
            if let Some(command) = &mut screen.command {
                command.engaged = true; command.scroll = 0; command.status = "Ready".into();
            }
            drop(screen);
            let _ = self.draw();
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
        let message = crate::inference::user_message(message);
        if self.screen.is_some() { self.log(message, Tone::Warning); return; }
        eprintln!("{} {}", self.paint("!", "1;33"), Self::clean(&crate::state::redact_credentials(message)));
    }

    pub fn error(&self, message: &str) {
        let message = crate::inference::user_message(message);
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

impl AgentEventSink for Terminal {
    fn emit(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Assistant { message } => Terminal::assistant(self, &message),
            AgentEvent::ModelReset => {
                if let Some(screen) = &self.screen {
                    let mut screen = screen.borrow_mut();
                    if let Some(start) = screen.stream_start.take() { screen.lines.truncate(start); }
                    screen.stream_buffer.clear();
                    drop(screen);
                    let _ = self.draw();
                } else if self.plain_stream_chars > 0 {
                    eprintln!();
                    self.plain_stream_chars = 0;
                }
            }
            AgentEvent::ModelText { text } => {
                if let Some(screen) = &self.screen {
                    let mut screen = screen.borrow_mut();
                    if screen.stream_start.is_none() { screen.stream_start = Some(screen.lines.len()); }
                    let start = screen.stream_start.unwrap_or(screen.lines.len());
                    screen.lines.truncate(start);
                    screen.stream_buffer = text.clone();
                    if screen.stream_buffer.len() > 32 * 1024 {
                        screen.stream_buffer = screen.stream_buffer.chars().rev()
                            .take(32 * 1024).collect::<String>().chars().rev().collect();
                    }
                    // Hold the unfinished word until its boundary is known so a
                    // credential split across model chunks is never printed early.
                    let visible_end = screen.stream_buffer.rfind(char::is_whitespace)
                        .map(|index| index + screen.stream_buffer[index..].chars().next()
                            .map(char::len_utf8).unwrap_or(0)).unwrap_or(0);
                    let safe = crate::state::redact_credentials(&screen.stream_buffer[..visible_end]);
                    Self::append_conversation_lines(&mut screen, &safe, Tone::Assistant);
                    screen.status = "Echo is responding".into();
                    screen.engaged = true;
                    if let Some(command) = &mut screen.command { command.engaged = true; command.scroll = 0; }
                    drop(screen);
                    let _ = self.draw();
                } else {
                    // One-shot output is printed by the completed assistant event.
                    // A progress mark keeps the shell responsive without exposing
                    // an incomplete credential token.
                    let before = self.plain_stream_chars / 80;
                    self.plain_stream_chars += text.chars().count();
                    if (before == 0 && self.plain_stream_chars == text.chars().count())
                        || self.plain_stream_chars / 80 > before {
                        eprint!("·");
                        let _ = io::stderr().flush();
                    }
                }
            }
            AgentEvent::ModelUsage { input_tokens, output_tokens, context_window, cost_usd } => {
                let mut detail = match (input_tokens, output_tokens) {
                    (Some(input), Some(output)) => format!("{input} in · {output} out"),
                    (Some(input), None) => format!("{input} input tokens"),
                    (None, Some(output)) => format!("{output} output tokens"),
                    (None, None) => "Model usage".into(),
                };
                if let Some(window) = context_window { detail.push_str(&format!(" · {window} context")); }
                if let Some(cost) = cost_usd { detail.push_str(&format!(" · ${cost:.4}")); }
                if let Some(screen) = &self.screen {
                    let mut screen = screen.borrow_mut();
                    if let Some(command) = &mut screen.command { command.status = detail.clone(); }
                    screen.status = detail;
                    drop(screen);
                    let _ = self.draw();
                }
            }
            AgentEvent::Info { message } => Terminal::info(self, &message),
            AgentEvent::Warning { message } => Terminal::warning(self, &message),
            AgentEvent::Error { message } => Terminal::error(self, &message),
            AgentEvent::Operation { detail } => Terminal::operation(self, &detail),
            AgentEvent::ToolRequest { tool, detail } => {
                Terminal::operation(self, &format!("{tool} · {detail}"));
            }
            AgentEvent::ToolResult { name, result, success } => {
                Terminal::tool_result(self, &name, &result, success);
            }
            AgentEvent::Proposal { label, details } => Terminal::proposal(self, &label, &details),
            AgentEvent::Raw { data } => Terminal::raw(self, &data),
            AgentEvent::Context { project, session, model } => {
                Terminal::context(self, &project, &session, &model);
            }
            AgentEvent::PolicyDecision { task_id, operation, decision, reason, policy, choices } => {
                let mut message = format!("Task {task_id} · {decision} · {operation}: {reason}");
                if let Some(policy) = policy { message.push_str(&format!(" (policy: {policy})")); }
                if !choices.is_empty() { message.push_str(". User decision required."); }
                if decision.eq_ignore_ascii_case("allowed") {
                    Terminal::info(self, &message);
                } else {
                    Terminal::warning(self, &message);
                }
            }
            AgentEvent::ApprovalRequested { description } => {
                Terminal::warning(self, &description);
            }
            AgentEvent::TaskState { task_id, state } => {
                Terminal::info(self, &format!("Task {task_id} · {state}"));
            }
            AgentEvent::Completed { summary } => Terminal::info(self, &summary),
            AgentEvent::Capability { id, action, status, message } => {
                if matches!(id.as_str(), "engineering_memory" | "experience_loop")
                    && action == "observe" { return; }
                if matches!(status.as_str(), "unavailable" | "failed" | "awaiting_user_decision") {
                    Terminal::warning(self, &message);
                } else if !message.is_empty() {
                    Terminal::info(self, &message);
                }
            }
        }
    }

    fn ask_approval(&mut self, description: &str) -> Result<bool> {
        Terminal::ask_approval(self, description)
    }

    fn ask_decision(&mut self, description: &str, choices: &[String]) -> Result<Option<String>> {
        self.ask_policy_decision(description, choices)
    }

    fn begin_busy(&mut self) -> Result<()> {
        if let Some(screen) = &self.screen {
            terminal::enable_raw_mode().context("enabling Echo input during model work")?;
            screen.borrow_mut().raw = true;
            self.draw()?;
        }
        Ok(())
    }

    fn begin_model_thinking(&mut self) -> Result<()> {
        if let Some(screen) = &self.screen {
            screen.borrow_mut().model_thinking.begin();
            self.redraw_footer_status()?;
        }
        Ok(())
    }

    fn end_model_thinking(&mut self) -> Result<()> {
        if let Some(screen) = &self.screen {
            screen.borrow_mut().model_thinking.end();
            self.redraw_footer_status()?;
        }
        Ok(())
    }

    fn poll_busy(&mut self) -> Option<crate::events::BusyInput> {
        let screen = self.screen.as_ref()?;
        let thinking_changed = screen.borrow_mut().model_thinking.tick();
        if thinking_changed { let _ = self.redraw_footer_status(); }
        if !event::poll(Duration::ZERO).ok()? { return None; }
        match event::read().ok()? {
            Event::Resize(_, _) => { let _ = self.draw(); None }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let mut screen = screen.borrow_mut();
                let result = match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        screen.input.clear(); screen.cursor = 0;
                        Some(crate::events::BusyInput::Cancel)
                    }
                    KeyCode::Enter => {
                        let line = std::mem::take(&mut screen.input);
                        screen.cursor = 0;
                        if line.trim() == "/cancel" { Some(crate::events::BusyInput::Cancel) }
                        else if line.trim().is_empty() { None }
                        else { Some(crate::events::BusyInput::Message(line)) }
                    }
                    KeyCode::Backspace if screen.cursor > 0 => {
                        let end = byte_index(&screen.input, screen.cursor);
                        let start = byte_index(&screen.input, screen.cursor - 1);
                        screen.input.replace_range(start..end, "");
                        screen.cursor -= 1; None
                    }
                    KeyCode::Left => { screen.cursor = screen.cursor.saturating_sub(1); None }
                    KeyCode::Right => {
                        screen.cursor = (screen.cursor + 1).min(screen.input.chars().count()); None
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && screen.input.len() < 16 * 1024 => {
                        let at = byte_index(&screen.input, screen.cursor);
                        screen.input.insert(at, ch); screen.cursor += 1; None
                    }
                    _ => None,
                };
                drop(screen);
                let _ = self.draw();
                result
            }
            _ => None,
        }
    }

    fn end_busy(&mut self) -> Result<()> {
        if let Some(screen) = &self.screen {
            terminal::disable_raw_mode().context("restoring Echo keyboard mode")?;
            screen.borrow_mut().raw = false;
        }
        Ok(())
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

fn session_activity(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Ready",
        SessionStatus::Running => "Task running",
        SessionStatus::WaitingApproval => "Approval needed",
        SessionStatus::AwaitingUserDecision => "Decision needed",
        SessionStatus::Interrupted => "Task interrupted",
        SessionStatus::Completed => "Task complete",
        SessionStatus::Failed => "Task failed",
        SessionStatus::Cancelled => "Task cancelled",
    }
}

/// Clip by terminal cells, keeping service-supplied text on a single row.
fn picker_clip(text: &str, width: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ")
        .chars().filter(|c| !c.is_control()).collect::<String>();
    if text.width() <= width { return text; }
    if width == 0 { return String::new(); }
    let mut result = String::new();
    let mut used = 0;
    for c in text.chars() {
        let cells = c.width().unwrap_or(0);
        if used + cells > width - 1 { break; }
        used += cells;
        result.push(c);
    }
    result.push('…');
    result
}

fn picker_name(text: &str, width: usize) -> String {
    // Keep the current/default marker visible even for a long model name.
    for suffix in [" (current)", " (default)"] {
        if let Some(name) = text.strip_suffix(suffix) {
            if width > suffix.len() {
                return picker_clip(name, width - suffix.len()) + suffix;
            }
        }
    }
    picker_clip(text, width)
}

fn render_model_picker(out: &mut impl Write, width: u16, height: u16, title: &str,
    choices: &[MenuChoice], selected: usize) -> io::Result<usize> {
    let w = usize::from(width);
    let h = usize::from(height);
    queue!(out, ResetColor, MoveTo(0, 0), Clear(ClearType::All), Hide)?;
    if w == 0 || h == 0 { out.flush()?; return Ok(0); }
    let top = if h >= 9 { 1 } else { 0 };
    queue!(out, MoveTo(1.min(width - 1), top), SetForegroundColor(Color::White),
        Print(picker_clip(title, w.saturating_sub(2))), ResetColor)?;
    if w < 16 || h < 5 {
        if h > 1 {
            queue!(out, MoveTo(0, height - 1), Print(picker_clip("Enlarge terminal", w.saturating_sub(1))))?;
        }
        out.flush()?; return Ok(0);
    }
    let start = if h >= 9 { 4 } else { 2 };
    let stacked = w < 44;
    let row_height = if stacked { 2 } else { 1 };
    let rows = (h.saturating_sub(start + 2) / row_height).max(1);
    let first = selected.saturating_sub(rows - 1).min(choices.len().saturating_sub(rows));
    let digits = choices.len().to_string().len();
    let prefix_width = digits + 3;
    let left_width = choices.iter().map(|c| picker_clip(&c.command, 120).width() + prefix_width)
        .max().unwrap_or(0).min(42).min(w * 45 / 100);
    for (index, choice) in choices.iter().enumerate().skip(first).take(rows) {
        let y = start + (index - first) * row_height;
        let active = index == selected;
        let foreground = if active { Color::Grey } else { Color::White };
        let emphasis = if active { Attribute::Reverse } else { Attribute::NoReverse };
        // Reverse video keeps selection visible even when NO_COLOR is set.
        queue!(out, MoveTo(0, y as u16), ResetColor, SetForegroundColor(foreground),
            SetAttribute(emphasis), Print(" ".repeat(w)), MoveTo(0, y as u16))?;
        let prefix = format!("{}{:>digits$}. ", if active { "›" } else { " " }, index + 1);
        let name_width = if stacked { w } else { left_width };
        let name = picker_name(&choice.command, name_width.saturating_sub(prefix_width));
        queue!(out, Print(prefix), Print(name))?;
        if stacked {
            queue!(out, MoveTo(0, (y + 1) as u16),
                SetForegroundColor(if active { Color::Grey } else { Color::DarkGrey }),
                SetAttribute(emphasis),
                Print(" ".repeat(w)), MoveTo(2, (y + 1) as u16),
                Print(picker_clip(&choice.description, w - 2)))?;
        } else {
            queue!(out, MoveTo((left_width + 2) as u16, y as u16),
                SetForegroundColor(if active { Color::Grey } else { Color::DarkGrey }),
                SetAttribute(emphasis),
                Print(picker_clip(&choice.description, w.saturating_sub(left_width + 3))))?;
        }
        queue!(out, SetAttribute(Attribute::NoReverse), ResetColor)?;
    }
    let visible = rows.min(choices.len());
    let footer_y = (start + visible * row_height + 1).min(h - 1);
    let footer = if choices.len() > rows {
        format!("enter select · esc back · ↑↓ / PgUp/PgDn · {}–{} of {}", first + 1, first + visible, choices.len())
    } else { "enter select · esc back".to_owned() };
    queue!(out, MoveTo(1, footer_y as u16), SetForegroundColor(Color::DarkGrey),
        Print(picker_clip(&footer, w - 2)), ResetColor)?;
    out.flush()?;
    Ok(rows)
}

fn truncate(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

fn session_picker_rows<'a>(entries: &'a [SessionPickerEntry], query: &str,
    cwd_only: bool, status_filter: u8, sort_created: bool) -> Vec<&'a SessionPickerEntry> {
    let query = query.to_lowercase();
    let mut rows: Vec<_> = entries.iter().filter(|entry| {
        if cwd_only && !entry.cwd_match { return false; }
        let active = matches!(&entry.status, SessionStatus::Idle | SessionStatus::Running
            | SessionStatus::WaitingApproval | SessionStatus::AwaitingUserDecision
            | SessionStatus::Interrupted);
        if (status_filter == 1 && !active) || (status_filter == 2 && active) { return false; }
        query.is_empty() || entry.title.to_lowercase().contains(query.as_str())
            || entry.project_name.to_lowercase().contains(query.as_str())
            || entry.id.to_lowercase().contains(query.as_str())
            || entry.directory.to_string_lossy().to_lowercase().contains(query.as_str())
    }).collect();
    rows.sort_by(|left, right| {
        let left_time = if sort_created { left.created_at_ms } else { left.updated_at_ms };
        let right_time = if sort_created { right.created_at_ms } else { right.updated_at_ms };
        right_time.cmp(&left_time).then_with(|| right.id.cmp(&left.id))
    });
    rows
}

fn session_status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "ready",
        SessionStatus::Running => "running",
        SessionStatus::WaitingApproval => "approval",
        SessionStatus::AwaitingUserDecision => "decision",
        SessionStatus::Interrupted => "paused",
        SessionStatus::Completed => "complete",
        SessionStatus::Failed => "failed",
        SessionStatus::Cancelled => "cancelled",
    }
}

fn relative_age(elapsed_ms: u128) -> String {
    let minutes = elapsed_ms / 60_000;
    if minutes == 0 { "now".into() }
    else if minutes < 60 { format!("{minutes}m ago") }
    else if minutes < 1_440 { format!("{}h ago", minutes / 60) }
    else if minutes < 525_600 { format!("{}d ago", minutes / 1_440) }
    else { format!("{}y ago", minutes / 525_600) }
}

fn format_help_lines(commands: &[crate::commands::CommandSpec], terminal_width: usize)
    -> Vec<(String, Tone)> {
    let width = terminal_width.saturating_sub(4).max(1);
    let label_width = commands.iter().map(|command| command.name.chars().count())
        .max().unwrap_or(0) + 2;
    let wide = width >= label_width + 20;
    let indent = if wide { label_width } else if width >= label_width {
        2.min(width.saturating_sub(1))
    } else { 0 };
    let description_width = width.saturating_sub(indent).max(1);
    let padding = " ".repeat(indent);
    let mut lines = Vec::new();
    for command in commands {
        let description = wrap_help_words(command.description, description_width);
        if wide {
            lines.push((format!("{:<label_width$}{}", command.name, description[0]), Tone::Assistant));
            for part in description.iter().skip(1) {
                lines.push((format!("{padding}{part}"), Tone::Assistant));
            }
        } else {
            lines.push((command.name.to_owned(), Tone::Brand));
            for part in description {
                lines.push((format!("{padding}{part}"), Tone::Assistant));
            }
        }
        for part in wrap_help_words(&format!("Usage: {}", command.name), description_width) {
            lines.push((format!("{padding}{part}"), Tone::Muted));
        }
        lines.push((String::new(), Tone::Muted));
    }
    lines
}

fn wrap_help_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut remainder = word;
        while remainder.chars().count() > width {
            if !line.is_empty() { lines.push(std::mem::take(&mut line)); }
            let split = byte_index(remainder, width);
            lines.push(remainder[..split].to_owned());
            remainder = &remainder[split..];
        }
        if remainder.is_empty() { continue; }
        if line.is_empty() {
            line.push_str(remainder);
        } else if line.chars().count() + 1 + remainder.chars().count() <= width {
            line.push(' ');
            line.push_str(remainder);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(remainder);
        }
    }
    if !line.is_empty() { lines.push(line); }
    if lines.is_empty() { lines.push(String::new()); }
    lines
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
