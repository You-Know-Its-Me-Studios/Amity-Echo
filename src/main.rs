mod agent;
mod config;
mod host;
mod model;
mod state;
mod terminal;

use agent::Agent;
use anyhow::{Context, Result, bail};
use config::Config;
use state::{MessageRole, Session, SessionStore};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use terminal::{Input, Terminal};

fn main() {
    if let Err(error) = run() {
        eprintln!("echocli: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut config = Config::load().context("loading Echo configuration")?;
    let cwd = env::current_dir().context("reading current directory")?;
    if config.projects.is_empty() {
        if let Some(host_config) = env::var_os("AMITY_HOST_CONFIG") {
            config.register_project(&cwd, PathBuf::from(host_config))?;
            config.save()?;
        }
    }
    if let Some(project) = config.discover_project(&cwd) {
        let id = project.id.clone();
        if config.active_project.as_deref() != Some(&id) {
            config.active_project = Some(id);
            config.save()?;
        }
    }
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") || args.first().map(String::as_str) == Some("help") {
        print_help();
        return Ok(());
    }
    if let Some(command) = args.first().map(String::as_str) {
        match command {
            "config" => return config_command(&mut config, &args[1..]),
            "project" | "projects" => return project_command(&mut config, &args[1..]),
            "model" | "models" => return model_command(&mut config, &args[1..]),
            _ => {}
        }
    }
    let store = SessionStore::new(&config).context("opening Echo session storage")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancel);
    ctrlc::set_handler(move || { signal.store(true, Ordering::SeqCst); })
        .context("installing Ctrl+C handler")?;
    let history_path = Config::home_dir().join("history.txt");
    let mut terminal = Terminal::new(history_path, config.terminal.color)?;
    match args.first().map(String::as_str) {
        None => {
            let session = current_session(&config, &store)?;
            interactive(&mut config, &store, &mut terminal, cancel.as_ref(), session)
        }
        Some("resume") => {
            let session = if let Some(id) = args.get(1) {
                store.load(id)?
            } else {
                let project = active_project(&config)?;
                store.latest(&project.id)?.context("no previous session for this project")?
            };
            interactive(&mut config, &store, &mut terminal, cancel.as_ref(), session)
        }
        Some("sessions") => list_sessions(&store, config.active_project.as_deref(), &terminal),
        Some("status") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.status()
        }
        Some("approve") => {
            if args.get(1).map(String::as_str) != Some("--yes") {
                bail!("approval requires `echocli approve --yes` or /approve in an interactive session");
            }
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.approve()
        }
        Some("cancel") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.cancel_task()
        }
        Some("continue") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.advance_task()
        }
        Some("interrupt") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.interrupt()
        }
        Some("review") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.review()
        }
        Some("raw") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.raw()
        }
        Some("tools") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.capabilities()
        }
        Some("report") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.report()
        }
        Some("reconcile") => {
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.reconcile()
        }
        Some("task") => {
            let instruction = args[1..].join(" ");
            if instruction.is_empty() { bail!("task instruction is required"); }
            let mut session = current_session(&config, &store)?;
            session.append_message(MessageRole::User, &instruction);
            let safe_instruction = session.messages.last().context("stored message is missing")?.content.clone();
            store.save(&session)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.start_task(&safe_instruction)
        }
        Some("task-steps") => {
            let path = args.get(1).context("task-steps requires a JSON plan file")?;
            let plan: serde_json::Value = serde_json::from_slice(&fs::read(path)
                .with_context(|| format!("reading task plan {path}"))?)?;
            let instruction = plan.get("request").and_then(serde_json::Value::as_str)
                .context("task plan requires a request string")?.to_owned();
            let steps = plan.get("steps").context("task plan requires steps")?.clone();
            let mut session = current_session(&config, &store)?;
            session.append_message(MessageRole::User, &instruction);
            let safe_instruction = session.messages.last().context("stored message is missing")?.content.clone();
            store.save(&session)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.start_task_steps(&safe_instruction, steps)
        }
        Some(_) => {
            let instruction = args.join(" ");
            let mut session = current_session(&config, &store)?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                terminal: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.instruction(&instruction)
        }
    }
}

fn active_project(config: &Config) -> Result<&config::ProjectRegistration> {
    config.active_project().context("no authorized project is registered; run `echocli project add <path> <amity-host.json>`")
}

fn current_session(config: &Config, store: &SessionStore) -> Result<Session> {
    let project = active_project(config)?;
    if let Some(session) = store.latest(&project.id)? { return Ok(session); }
    store.create(&project.id, project.selected_model.clone()).map_err(Into::into)
}

fn interactive(config: &mut Config, store: &SessionStore, terminal: &mut Terminal,
    cancel: &AtomicBool, mut session: Session) -> Result<()> {
    if !config.projects.iter().any(|project| project.id == session.project_id) {
        bail!("session project is no longer registered");
    }
    terminal.banner(&session.project_id, &session.id, session.model.as_deref().unwrap_or("default"));
    if let Some(task_id) = session.active_task.as_deref() {
        terminal.info(&format!("Linked Amity task: {task_id}. Use /status or /continue."));
    }
    loop {
        cancel.store(false, Ordering::Relaxed);
        match terminal.read_line(&session.project_id, &session.id)? {
            Input::Eof => return Ok(()),
            Input::Interrupt => { cancel.store(false, Ordering::Relaxed); terminal.info("Input interrupted. Use /cancel to cancel an Amity task."); }
            Input::Line(line) => {
                let line = line.trim();
                if line.is_empty() { continue; }
                if line == "/exit" || line == "/quit" { return Ok(()); }
                if line == "/help" { print_interactive_help(); continue; }
                if line == "/sessions" { list_sessions(store, Some(&session.project_id), terminal)?; continue; }
                if line == "/projects" { list_projects(config); continue; }
                if line == "/models" { list_models(config); continue; }
                if line == "/config" { show_config(config)?; continue; }
                if let Some(id) = line.strip_prefix("/resume ") {
                    let result = (|| -> Result<()> {
                        let next = store.load(id.trim())?;
                        if next.project_id != session.project_id { config.select_project(&next.project_id)?; config.save()?; }
                        session = next;
                        terminal.context(&session.project_id, &session.id, session.model.as_deref().unwrap_or("default"));
                        Ok(())
                    })();
                    if let Err(error) = result { terminal.error(&format!("{error:#}")); }
                    continue;
                }
                if line == "/session new" {
                    let result = (|| -> Result<()> {
                        let project = active_project(config)?;
                        session = store.create(&project.id, project.selected_model.clone())?;
                        terminal.context(&session.project_id, &session.id, session.model.as_deref().unwrap_or("default"));
                        Ok(())
                    })();
                    if let Err(error) = result { terminal.error(&format!("{error:#}")); }
                    continue;
                }
                if let Some(id) = line.strip_prefix("/project ") {
                    let result = (|| -> Result<()> {
                        let id = config.select_project(id.trim())?.id.clone();
                        config.save()?;
                        let project = active_project(config)?;
                        session = if let Some(existing) = store.latest(&id)? { existing }
                            else { store.create(&id, project.selected_model.clone())? };
                        terminal.context(&session.project_id, &session.id, session.model.as_deref().unwrap_or("default"));
                        Ok(())
                    })();
                    if let Err(error) = result { terminal.error(&format!("{error:#}")); }
                    continue;
                }
                if let Some(name) = line.strip_prefix("/model ") {
                    let result = (|| -> Result<()> {
                        let model = if name.trim() == "default" { None } else { Some(name.trim()) };
                        config.active_project_mut().context("no active project")?.select_model(model)?;
                        config.save()?;
                        session.model = model.map(str::to_owned);
                        store.save(&session)?;
                        terminal.info(&format!("Selected Amity model profile {}", model.unwrap_or("default")));
                        Ok(())
                    })();
                    if let Err(error) = result { terminal.error(&format!("{error:#}")); }
                    continue;
                }
                let mut agent = Agent { config, store, session: &mut session, terminal, cancel, interactive: true };
                let result = match line {
                    "/status" => agent.status(),
                    "/review" => agent.review(),
                    "/approve" => agent.approve(),
                    "/cancel" => agent.cancel_task(),
                    "/retry" => agent.retry(),
                    "/interrupt" => agent.interrupt(),
                    "/continue" => agent.advance_task(),
                    "/raw" => agent.raw(),
                    "/tools" => agent.capabilities(),
                    "/report" => agent.report(),
                    "/reconcile" => agent.reconcile(),
                    _ if line.starts_with("/task ") => {
                        let instruction = line.trim_start_matches("/task ").trim();
                        agent.session.append_message(MessageRole::User, instruction);
                        let safe_instruction = agent.session.messages.last().context("stored message is missing")?.content.clone();
                        agent.store.save(agent.session)?;
                        agent.start_task(&safe_instruction)
                    }
                    _ if line.starts_with("/task-steps ") => {
                        (|| -> Result<()> {
                            let path = line.trim_start_matches("/task-steps ").trim();
                            let plan: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
                            let instruction = plan.get("request").and_then(serde_json::Value::as_str)
                                .context("task plan requires a request string")?.to_owned();
                            let steps = plan.get("steps").context("task plan requires steps")?.clone();
                            agent.session.append_message(MessageRole::User, &instruction);
                            let safe_instruction = agent.session.messages.last().context("stored message is missing")?.content.clone();
                            agent.store.save(agent.session)?;
                            agent.start_task_steps(&safe_instruction, steps)
                        })()
                    }
                    _ if line.starts_with('/') => { agent.terminal.warning("Unknown command. Type /help."); Ok(()) }
                    _ => agent.instruction(line),
                };
                if let Err(error) = result { agent.terminal.error(&format!("{error:#}")); }
            }
        }
    }
}

fn list_sessions(store: &SessionStore, project_id: Option<&str>, terminal: &Terminal) -> Result<()> {
    let sessions = store.list(project_id)?;
    if sessions.is_empty() { terminal.info("No sessions found."); return Ok(()); }
    for session in sessions {
        terminal.raw(&format!("{}  {}  {:?}  task {}  {} messages",
            session.id, session.project_id, session.status,
            session.active_task.as_deref().unwrap_or("—"), session.messages.len()));
    }
    Ok(())
}

fn list_projects(config: &Config) {
    for project in &config.projects {
        let mark = if config.active_project.as_deref() == Some(&project.id) { "*" } else { " " };
        println!("{mark} {}  {}  {}", project.id, project.path.display(), project.host_config.display());
    }
}

fn list_models(config: &Config) {
    if let Some(project) = config.active_project() {
        println!("{} default  {}", if project.selected_model.is_none() { "*" } else { " " }, project.host_config.display());
        for model in &project.models {
            println!("{} {}  {}", if project.selected_model.as_deref() == Some(&model.name) { "*" } else { " " }, model.name, model.host_config.display());
        }
    }
}

fn show_config(config: &Config) -> Result<()> {
    println!("Echo config: {}", Config::path().display());
    println!("{}", serde_json::to_string_pretty(config)?);
    Ok(())
}

fn config_command(config: &mut Config, args: &[String]) -> Result<()> {
    match args {
        [] => show_config(config),
        [action, path] if action == "host" => {
            config.host_executable = PathBuf::from(path);
            config.save()?;
            show_config(config)
        }
        [action, value] if action == "timeout" => {
            let seconds: u64 = value.parse().context("timeout must be seconds")?;
            if seconds == 0 { bail!("timeout must be positive"); }
            config.resources.host_timeout_seconds = seconds;
            config.save()?;
            show_config(config)
        }
        [action, value] if action == "color" => {
            config.terminal.color = parse_bool(value)?;
            config.save()?;
            show_config(config)
        }
        [action, value] if action == "ask-before-execution" => {
            config.execution.ask_before_execution = parse_bool(value)?;
            config.save()?;
            show_config(config)
        }
        _ => bail!("usage: echocli config [host <executable> | timeout <seconds> | color <on|off> | ask-before-execution <on|off>]"),
    }
}

fn project_command(config: &mut Config, args: &[String]) -> Result<()> {
    match args {
        [] => { list_projects(config); Ok(()) }
        [list] if list == "list" => { list_projects(config); Ok(()) }
        [action, path, host_config] if action == "add" => {
            let id = config.register_project(path, host_config)?.id.clone();
            config.save()?;
            println!("Registered project {id}");
            Ok(())
        }
        [action, id] if action == "use" => {
            let id = config.select_project(id)?.id.clone();
            config.save()?;
            println!("Selected project {id}");
            Ok(())
        }
        _ => bail!("usage: echocli project [list | add <directory> <amity-host.json> | use <id>]"),
    }
}

fn model_command(config: &mut Config, args: &[String]) -> Result<()> {
    match args {
        [] => { list_models(config); Ok(()) }
        [list] if list == "list" => { list_models(config); Ok(()) }
        [action, name, host_config] if action == "add" => {
            active_project(config)?;
            config.active_project_mut().context("no active project")?.add_model(name, host_config)?;
            config.save()?;
            println!("Added model profile {name}");
            Ok(())
        }
        [action, name] if action == "use" => {
            let name = if name == "default" { None } else { Some(name.as_str()) };
            config.active_project_mut().context("no active project")?.select_model(name)?;
            config.save()?;
            let project_id = active_project(config)?.id.clone();
            let store = SessionStore::new(config)?;
            if let Some(mut session) = store.latest(&project_id)? {
                session.model = name.map(str::to_owned);
                store.save(&session)?;
            }
            println!("Selected model profile {}", name.unwrap_or("default"));
            Ok(())
        }
        _ => bail!("usage: echocli model [list | add <name> <amity-host.json> | use <name|default>]"),
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value { "on" | "true" => Ok(true), "off" | "false" => Ok(false), _ => bail!("expected on or off") }
}

fn print_help() {
    println!("Amity Echo · first-party coding agent\n\
echocli                          Interactive agent\n\
echocli \"instruction\"            Ask or request a coding task\n\
echocli task \"instruction\"       Force an Amity coding task\n\
echocli task-steps <plan.json>    Run a structured multistep Amity task\n\
echocli resume [session-id]      Reopen a session\n\
echocli sessions                 List project sessions\n\
echocli status                   Inspect active task\n\
echocli review                   Show pending gateway proposal\n\
echocli approve --yes            Explicitly approve and execute pending proposal\n\
echocli continue | interrupt | cancel | raw  Manage or inspect active task\n\
echocli tools | report | reconcile Inspect host capabilities and evidence\n\
echocli project add <path> <amity-host.json>\n\
echocli project use <id> | project list\n\
echocli model add <name> <amity-host.json>\n\
echocli model use <name|default> | model list\n\
echocli config [host <path> | timeout <seconds> | color <on|off> | ask-before-execution <on|off>]\n\
AMITY_HOST_TOKEN is read from the environment and never stored by Echo.\n\
AMITY_HOST_CONFIG can register the current directory on first launch.");
}

fn print_interactive_help() {
    println!("/help  /status  /review  /approve  /continue  /retry  /interrupt  /cancel  /raw\n\
/tools  /report  /reconcile  /task-steps <plan.json>\n\
/sessions  /resume <id>  /session new  /projects  /project <id>\n\
/models  /model <name|default>  /config  /task <instruction>  /exit");
}
