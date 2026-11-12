mod agent;
mod commands;
mod config;
mod events;
mod host;
mod model;
mod state;
mod terminal;
mod update;

use agent::Agent;
use anyhow::{Context, Result, bail};
use commands::{Command, COMMANDS};
use config::Config;
use state::{MessageRole, Session, SessionStatus, SessionStore};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use terminal::{Input, MenuChoice, SessionPickerEntry, Terminal};

fn main() {
    if let Err(error) = run() {
        eprintln!("echocli: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [version] if matches!(version.as_str(), "--version" | "-V") => {
            println!("Amity Echo {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        [help] if matches!(help.as_str(), "--help" | "-h") => {
            print_help();
            return Ok(());
        }
        [update] if update == "update" => return update::run(false),
        [update, check] if update == "update" && check == "--check" => return update::run(true),
        [update, ..] if update == "update" => bail!("Usage: echocli update [--check]"),
        _ => {}
    }

    let mut config = Config::load().context("loading Echo settings")?;
    let cwd = env::current_dir().context("reading current directory")?;
    let mut registration_error = None;
    if config.projects.is_empty() {
        if let Some(host_config) = env::var_os("AMITY_HOST_CONFIG") {
            match config.register_host_config(PathBuf::from(host_config)) {
                Ok(_) => config.save()?,
                Err(error) => registration_error = Some(error.to_string()),
            }
        }
    }
    if let Some(project) = config.discover_project(&cwd) {
        let id = project.id.clone();
        if config.active_project.as_deref() != Some(&id) {
            config.active_project = Some(id);
            config.save()?;
        }
    }

    let store = SessionStore::new(&config).context("opening Echo conversations")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancel);
    ctrlc::set_handler(move || { signal.store(true, Ordering::SeqCst); })
        .context("installing Ctrl+C handler")?;
    let mut terminal = Terminal::new(Config::home_dir().join("history.txt"), config.terminal.color)?;
    if let Some(error) = registration_error {
        terminal.warning(&format!("Could not connect the project from AMITY_HOST_CONFIG: {error}"));
    }

    match args.as_slice() {
        [] => {
            terminal.enter_interactive(&cwd)?;
            let session = if config.active_project().is_some() {
                Some(current_session(&config, &store)?)
            } else { None };
            interactive(&mut config, &store, &mut terminal, cancel.as_ref(), session)
        }
        [command] if command == "resume" || command == "sessions" => {
            terminal.enter_interactive(&cwd)?;
            let Some(session) = browse_session(&config, &store, &mut terminal)? else { return Ok(()); };
            select_session_project(&mut config, &session)?;
            interactive(&mut config, &store, &mut terminal, cancel.as_ref(), Some(session))
        }
        [command] if command == "status" => {
            if config.active_project().is_none() {
                terminal.info("No project is connected. Open Echo and choose /project to connect one.");
                return Ok(());
            }
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                events: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.status()
        }
        [command, ..] if matches!(command.as_str(), "resume" | "sessions" | "status") =>
            bail!("Open `echocli {command}` without extra arguments."),
        [command, ..] if matches!(command.as_str(),
            "config" | "project" | "projects" | "model" | "models" | "approve"
            | "decision" | "continue" | "interrupt" | "review" | "raw" | "tools"
            | "report" | "reconcile" | "task" | "task-steps" | "cancel" | "retry"
            | "version" | "help") =>
            bail!("`echocli {command}` is no longer a shell command. Open Echo and use /help for the available actions."),
        [first, ..] if first.starts_with('-') || first.starts_with('/') =>
            bail!("Unknown option `{first}`. Run `echocli --help` for usage."),
        _ => {
            let instruction = args.join(" ");
            if config.active_project().is_none() {
                bail!("No project is connected. Open `echocli`, choose /project, and connect an Amity project.");
            }
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                events: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.instruction(&instruction)
        }
    }
}

fn active_project(config: &Config) -> Result<&config::ProjectRegistration> {
    config.active_project().context("No project is connected. Open Echo and choose /project.")
}

fn current_session(config: &Config, store: &SessionStore) -> Result<Session> {
    let project = active_project(config)?;
    let cwd = env::current_dir()?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    if let Some(session) = store.list(Some(&project.id))?.into_iter()
        .find(|session| session_matches_cwd(session, &cwd, &project.path)) {
        return Ok(session);
    }
    store.create(&project.id, project.selected_model.clone()).map_err(Into::into)
}

fn interactive(config: &mut Config, store: &SessionStore, terminal: &mut Terminal,
    cancel: &AtomicBool, mut session: Option<Session>) -> Result<()> {
    show_session(terminal, session.as_ref());
    if let Some(current) = session.as_mut() {
        resume_pending(config, store, terminal, cancel, current);
    }
    loop {
        cancel.store(false, Ordering::Relaxed);
        let project_label = session.as_ref().map(|value| value.project_id.as_str()).unwrap_or("Local workspace");
        let session_label = session.as_ref().map(|value| value.id.as_str()).unwrap_or("new");
        let input = terminal.read_line(project_label, session_label)?;
        let line = match input {
            Input::Eof | Input::Interrupt => return Ok(()),
            Input::Line(line) => line,
        };
        let line = line.trim();
        if line.is_empty() { continue; }
        if line.starts_with('/') {
            let Some(command) = commands::find(line) else {
                terminal.warning("Unknown command. Type /help to see available commands.");
                continue;
            };
            let result = match command.id {
                Command::Help => { print_interactive_help(terminal); Ok(()) }
                Command::Exit => return Ok(()),
                Command::Project => {
                    (|| -> Result<()> {
                        if choose_project(config, terminal)? {
                            session = Some(current_session(config, store)?);
                            show_session(terminal, session.as_ref());
                            if let Some(current) = session.as_mut() {
                                resume_pending(config, store, terminal, cancel, current);
                            }
                        }
                        Ok(())
                    })()
                }
                Command::Sessions => {
                    (|| -> Result<()> {
                        if let Some(next) = browse_session(config, store, terminal)? {
                            select_session_project(config, &next)?;
                            session = Some(next);
                            show_session(terminal, session.as_ref());
                            if let Some(current) = session.as_mut() {
                                resume_pending(config, store, terminal, cancel, current);
                            }
                        }
                        Ok(())
                    })()
                }
                Command::NewSession => {
                    (|| -> Result<()> {
                        if let Some(project) = config.active_project() {
                            session = Some(store.create(&project.id, project.selected_model.clone())?);
                            show_session(terminal, session.as_ref());
                        } else {
                            terminal.info("Connect a project with /project before starting a conversation.");
                        }
                        Ok(())
                    })()
                }
                Command::Model => {
                    if let Some(current) = session.as_mut() {
                        choose_model(config, store, terminal, current)
                    } else {
                        terminal.info("Connect a project with /project before choosing a model.");
                        Ok(())
                    }
                }
                Command::Settings => choose_settings(config, terminal),
                Command::Status => {
                    if let Some(current) = session.as_mut() {
                        let mut agent = Agent { config, store, session: current, events: terminal,
                            cancel, interactive: true };
                        agent.status()
                    } else {
                        terminal.info("Echo is ready. Connect a project with /project to start coding.");
                        Ok(())
                    }
                }
                Command::ReviewChanges => {
                    if let Some(current) = session.as_mut() {
                        let mut agent = Agent { config, store, session: current, events: terminal,
                            cancel, interactive: true };
                        agent.review()
                    } else {
                        terminal.info("No changes to review. Connect a project with /project.");
                        Ok(())
                    }
                }
                Command::Cancel => {
                    if let Some(current) = session.as_mut() {
                        if matches!(&current.status, SessionStatus::Running | SessionStatus::WaitingApproval
                            | SessionStatus::AwaitingUserDecision | SessionStatus::Interrupted | SessionStatus::Failed)
                            && (current.active_task.is_some() || current.pending_policy_change.is_some()) {
                            let mut agent = Agent { config, store, session: current, events: terminal,
                                cancel, interactive: true };
                            agent.cancel_task()
                        } else {
                            terminal.info("There is no active operation to cancel.");
                            Ok(())
                        }
                    } else {
                        terminal.info("There is no active operation to cancel.");
                        Ok(())
                    }
                }
            };
            if let Err(error) = result { terminal.error(&format!("{error:#}")); }
            continue;
        }
        if let Some(current) = session.as_mut() {
            let mut agent = Agent { config, store, session: current, events: terminal,
                cancel, interactive: true };
            if let Err(error) = agent.instruction(line) { agent.events.error(&format!("{error:#}")); }
        } else {
            terminal.info("Connect a project with /project, then send your request again.");
        }
    }
}

fn show_session(terminal: &Terminal, session: Option<&Session>) {
    if let Some(session) = session {
        terminal.context(&session.project_id, &session.id, session.model.as_deref().unwrap_or("default"));
        terminal.load_session(session);
    } else {
        terminal.banner("Local workspace", "new", "not connected");
    }
}

fn resume_pending(config: &Config, store: &SessionStore, terminal: &mut Terminal,
    cancel: &AtomicBool, session: &mut Session) {
    let result = match session.status.clone() {
        SessionStatus::AwaitingUserDecision => {
            let mut agent = Agent { config, store, session, events: terminal, cancel, interactive: true };
            agent.instruction("")
        }
        SessionStatus::WaitingApproval => {
            let mut agent = Agent { config, store, session, events: terminal, cancel, interactive: true };
            agent.review()
        }
        SessionStatus::Interrupted | SessionStatus::Failed if session.active_task.is_some() => {
            let options = [
                MenuChoice { command: "Resume work".into(), description: "Continue the saved task".into() },
                MenuChoice { command: "Cancel work".into(), description: "Stop the saved task".into() },
                MenuChoice { command: "Later".into(), description: "Keep it for another time".into() },
            ];
            match terminal.choose_option("Continue this conversation?", &options) {
                Ok(Some(0)) => {
                    let mut agent = Agent { config, store, session, events: terminal, cancel, interactive: true };
                    agent.retry()
                }
                Ok(Some(1)) => {
                    let mut agent = Agent { config, store, session, events: terminal, cancel, interactive: true };
                    agent.cancel_task()
                }
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            }
        }
        _ => Ok(()),
    };
    if let Err(error) = result { terminal.error(&format!("{error:#}")); }
}

fn choose_project(config: &mut Config, terminal: &mut Terminal) -> Result<bool> {
    let mut options: Vec<MenuChoice> = config.projects.iter().map(|project| MenuChoice {
        command: project.name.clone(),
        description: format!("{}{}", project.path.display(),
            if config.active_project.as_deref() == Some(&project.id) { "  ·  current" } else { "" }),
    }).collect();
    options.push(MenuChoice { command: "Connect a project".into(),
        description: "Choose its Amity connection file".into() });
    let Some(index) = terminal.choose_option("Choose a project", &options)? else { return Ok(false); };
    if index == config.projects.len() {
        let Some(path) = terminal.prompt_text("Path to this project's Amity connection JSON file")? else {
            return Ok(false);
        };
        if path.trim().is_empty() { return Ok(false); }
        let mut next = config.clone();
        let id = next.register_host_config(path.trim().trim_matches('"'))?.id.clone();
        next.save()?;
        *config = next;
        terminal.info(&format!("Connected project {id}."));
    } else {
        let id = config.projects[index].id.clone();
        if config.active_project.as_deref() == Some(&id) { return Ok(false); }
        let mut next = config.clone();
        next.select_project(&id)?;
        next.save()?;
        *config = next;
        terminal.info(&format!("Switched to {}.", config.projects[index].name));
    }
    Ok(true)
}

fn select_session_project(config: &mut Config, session: &Session) -> Result<()> {
    if config.active_project.as_deref() != Some(&session.project_id) {
        config.select_project(&session.project_id)?;
        config.save()?;
    }
    Ok(())
}

fn choose_model(config: &mut Config, store: &SessionStore, terminal: &mut Terminal,
    session: &mut Session) -> Result<()> {
    let models = active_project(config)?.models.clone();
    let mut options = vec![MenuChoice {
        command: "Project default".into(),
        description: if session.model.is_none() { "Current model".into() }
            else { "Use the model in this project's Amity connection".into() },
    }];
    for model in &models {
        options.push(MenuChoice { command: model.name.clone(),
            description: if session.model.as_deref() == Some(&model.name) {
                "Current model".into()
            } else { "Configured model".into() } });
    }
    options.push(MenuChoice { command: "Add a model".into(),
        description: "Connect another model for this project".into() });
    let Some(index) = terminal.choose_option("Choose an AI model", &options)? else { return Ok(()); };
    let mut next_config = config.clone();
    let selected = if index == options.len() - 1 {
        let Some(name) = terminal.prompt_text("Name for this model")? else { return Ok(()); };
        if name.trim().is_empty() { bail!("Enter a model name."); }
        let Some(path) = terminal.prompt_text("Path to its Amity connection JSON file")? else {
            return Ok(());
        };
        if path.trim().is_empty() { bail!("Enter the connection file path."); }
        next_config.active_project_mut().context("No project is selected")?
            .add_model(name.trim(), path.trim().trim_matches('"'))?;
        Some(name.trim().to_owned())
    } else if index == 0 { None } else {
        Some(models[index - 1].name.clone())
    };
    next_config.active_project_mut().context("No project is selected")?
        .select_model(selected.as_deref())?;
    next_config.save()?;
    let mut next_session = session.clone();
    next_session.model = selected.clone();
    store.save(&next_session)?;
    *config = next_config;
    *session = next_session;
    terminal.context(&session.project_id, &session.id, selected.as_deref().unwrap_or("default"));
    terminal.info(&format!("Using {}.", selected.as_deref().unwrap_or("the project's default model")));
    Ok(())
}

fn choose_settings(config: &mut Config, terminal: &mut Terminal) -> Result<()> {
    loop {
        let options = vec![
            MenuChoice { command: "Amity app location".into(),
                description: config.host_executable.display().to_string() },
            MenuChoice { command: "Terminal colors".into(),
                description: on_off(config.terminal.color).into() },
            MenuChoice { command: "Detailed operation results".into(),
                description: on_off(config.terminal.show_tool_details).into() },
            MenuChoice { command: "Ask before execution".into(),
                description: on_off(config.execution.ask_before_execution).into() },
            MenuChoice { command: "Continue task automatically".into(),
                description: on_off(config.execution.auto_continue).into() },
            MenuChoice { command: "Time to wait for Amity".into(),
                description: format!("{} seconds", config.resources.host_timeout_seconds) },
            MenuChoice { command: "Maximum work rounds".into(),
                description: config.resources.max_agent_turns.to_string() },
            MenuChoice { command: "Conversation memory length".into(),
                description: format!("{} messages", config.resources.max_history_messages) },
        ];
        let Some(index) = terminal.choose_option("Echo settings", &options)? else { return Ok(()); };
        let mut next = config.clone();
        let mut changed = true;
        match index {
            0 => {
                if let Some(value) = terminal.prompt_text("Path or command used to open Amity")? {
                    if !value.trim().is_empty() { next.host_executable = PathBuf::from(value.trim().trim_matches('"')); }
                    else { changed = false; }
                } else { changed = false; }
            }
            1 => next.terminal.color = !next.terminal.color,
            2 => next.terminal.show_tool_details = !next.terminal.show_tool_details,
            3 => next.execution.ask_before_execution = !next.execution.ask_before_execution,
            4 => next.execution.auto_continue = !next.execution.auto_continue,
            5 => if let Some(value) = terminal.prompt_text("Timeout in seconds")? {
                if !value.trim().is_empty() { next.resources.host_timeout_seconds = positive(&value)?; }
                else { changed = false; }
            } else { changed = false; },
            6 => if let Some(value) = terminal.prompt_text("Maximum work rounds per request")? {
                if !value.trim().is_empty() { next.resources.max_agent_turns = usize::try_from(positive(&value)?)
                    .context("Number is too large for this computer")?; }
                else { changed = false; }
            } else { changed = false; },
            7 => if let Some(value) = terminal.prompt_text("Maximum conversation messages sent to the model")? {
                if !value.trim().is_empty() { next.resources.max_history_messages = usize::try_from(positive(&value)?)
                    .context("Number is too large for this computer")?; }
                else { changed = false; }
            } else { changed = false; },
            _ => unreachable!(),
        }
        if !changed { continue; }
        next.save()?;
        *config = next;
        if index == 1 { terminal.set_color(config.terminal.color); }
        terminal.info("Setting saved.");
    }
}

fn positive(value: &str) -> Result<u64> {
    let parsed: u64 = value.trim().parse().context("Enter a whole number greater than zero")?;
    if parsed == 0 { bail!("Enter a number greater than zero."); }
    Ok(parsed)
}

fn on_off(value: bool) -> &'static str { if value { "On" } else { "Off" } }

fn browse_session(config: &Config, store: &SessionStore, terminal: &mut Terminal) -> Result<Option<Session>> {
    let entries = session_picker_entries(config, store)?;
    match terminal.choose_session(&entries)? {
        Some(id) => Ok(Some(store.load(&id)?)),
        None => Ok(None),
    }
}

fn session_picker_entries(config: &Config, store: &SessionStore) -> Result<Vec<SessionPickerEntry>> {
    let cwd = env::current_dir()?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let mut entries = Vec::new();
    for session in store.list(None)? {
        let Some(project) = config.projects.iter().find(|project| project.id == session.project_id) else { continue; };
        let directory = session.working_directory.as_deref().unwrap_or(&project.path).to_path_buf();
        let cwd_match = session_matches_cwd(&session, &cwd, &project.path);
        let title = session.messages.iter()
            .find(|message| matches!(&message.role, MessageRole::User))
            .and_then(|message| message.content.lines().find(|line| !line.trim().is_empty()))
            .map(|line| line.trim().chars().take(120).collect())
            .unwrap_or_else(|| "New conversation".to_owned());
        entries.push(SessionPickerEntry {
            id: session.id, title, project_name: project.name.clone(), directory, cwd_match,
            created_at_ms: session.created_at_ms, updated_at_ms: session.updated_at_ms,
            status: session.status,
        });
    }
    Ok(entries)
}

fn same_directory(left: &Path, right: &Path) -> bool {
    left == right || cfg!(windows)
        && left.to_string_lossy().eq_ignore_ascii_case(right.to_string_lossy().as_ref())
}

fn session_matches_cwd(session: &Session, cwd: &Path, project_root: &Path) -> bool {
    match session.working_directory.as_deref() {
        Some(directory) => same_directory(directory, cwd),
        None => cwd.starts_with(project_root),
    }
}

fn print_help() {
    println!("Amity Echo · coding agent\n\
echocli                         Open Echo\n\
echocli \"instruction\"           Ask Echo to work in this project\n\
echocli resume                  Browse and reopen conversations\n\
echocli sessions                Browse conversations\n\
echocli status                  See what Echo is doing\n\
echocli update [--check]        Install or check for an update\n\
echocli --version               Show the installed version\n\
echocli --help                  Show this help\n\nInside Echo:");
    for command in COMMANDS {
        println!("  {:<18} {}", command.name, command.description);
    }
}

fn print_interactive_help(terminal: &Terminal) {
    for command in COMMANDS {
        terminal.info(&format!("{:<18} {}", command.name, command.description));
    }
}
