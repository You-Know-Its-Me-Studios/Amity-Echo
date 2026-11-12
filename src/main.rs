mod agent;
mod attachments;
mod bridge;
mod capabilities;
mod commands;
mod config;
mod events;
mod headless;
mod ide;
mod host;
mod model;
mod skills;
mod state;
mod subagents;
mod terminal;
mod update;
mod workspace;

use agent::Agent;
use anyhow::{Context, Result, bail};
use commands::{Command, COMMANDS};
use config::Config;
use state::{MessageRole, Session, SessionStatus, SessionStore};
use std::env;
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use terminal::{Input, MenuChoice, SessionPickerEntry, Terminal};

fn main() {
    if let Err(error) = run() {
        let message = state::redact_credentials(&format!("{error:#}"));
        if env::args().any(|argument| argument == "--json") {
            println!("{}", serde_json::json!({"type":"result","status":"failed",
                "exit_code":1,"error":message}));
        } else { eprintln!("echocli: {message}"); }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let mut args = Vec::new();
    let mut json_mode = false;
    let mut attachment_paths = Vec::new();
    let mut editor_context_path = None;
    let mut index = 0;
    while index < raw_args.len() {
        match raw_args[index].as_str() {
            "--json" => json_mode = true,
            "--attach" => {
                index += 1;
                attachment_paths.push(PathBuf::from(raw_args.get(index)
                    .context("--attach requires a file path")?));
            }
            "--editor-context" => {
                index += 1;
                editor_context_path = Some(PathBuf::from(raw_args.get(index)
                    .context("--editor-context requires a JSON file path")?));
            }
            _ => args.push(raw_args[index].clone()),
        }
        index += 1;
    }
    if args.first().is_some_and(|argument| argument == "__model_bridge") {
        return bridge::run(&args[1..]);
    }
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
    let old_project = config.active_project.clone();
    let old_count = config.projects.len();
    config.select_working_directory(&cwd)?;
    if config.active_project != old_project || config.projects.len() != old_count {
        config.save()?;
    }

    let store = SessionStore::new(&config).context("opening Echo conversations")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancel);
    ctrlc::set_handler(move || { signal.store(true, Ordering::SeqCst); })
        .context("installing Ctrl+C handler")?;
    if json_mode {
        if args.is_empty() { bail!("Usage: echocli --json <instruction>"); }
        let instruction = args.join(" ");
        let mut sink = headless::JsonSink::new();
        let mut session = current_session(&config, &store)?;
        attach_inputs(&mut session, &store, &config, &attachment_paths,
            editor_context_path.as_deref())?;
        let result = {
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                events: &mut sink, cancel: cancel.as_ref(), interactive: false };
            agent.instruction(&instruction)
        };
        let code = sink.finish(&session, result.as_ref().err())?;
        if code != 0 { std::process::exit(code); }
        return Ok(());
    }
    let mut terminal = Terminal::new(Config::home_dir().join("history.txt"), config.terminal.color)?;
    if let Some(error) = registration_error {
        terminal.warning(&format!("Could not connect the project from AMITY_HOST_CONFIG: {error}"));
    }

    match args.as_slice() {
        [] => {
            terminal.enter_interactive(&cwd)?;
            let mut session = Some(current_session(&config, &store)?);
            if let Some(current) = session.as_mut() {
                attach_inputs(current, &store, &config, &attachment_paths,
                    editor_context_path.as_deref())?;
            }
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
                terminal.info("Echo could not select the current working directory.");
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
                bail!("Echo could not select the current working directory");
            }
            let mut session = current_session(&config, &store)?;
            attach_inputs(&mut session, &store, &config, &attachment_paths,
                editor_context_path.as_deref())?;
            let mut agent = Agent { config: &config, store: &store, session: &mut session,
                events: &mut terminal, cancel: cancel.as_ref(), interactive: false };
            agent.instruction(&instruction)
        }
    }
}

fn attach_inputs(session: &mut Session, store: &SessionStore, config: &Config,
    files: &[PathBuf], editor_context: Option<&Path>) -> Result<()> {
    if session.pending_attachments.len() + files.len() > 2 {
        bail!("attach at most two files to a message");
    }
    for path in files { session.pending_attachments.push(attachments::attach(path)?); }
    if let Some(path) = editor_context {
        session.pending_editor_context = Some(ide::load(path, active_project(config)?)?);
    }
    if !files.is_empty() { store.save(session)?; }
    Ok(())
}

fn active_project(config: &Config) -> Result<&config::ProjectRegistration> {
    config.active_project().context("Echo could not select the current working directory")
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
    let mut steward = StewardMonitor::new(config);
    show_session(config, terminal, session.as_ref());
    if let Some(current) = session.as_mut() {
        resume_pending(config, store, terminal, cancel, current);
    }
    loop {
        cancel.store(false, Ordering::Relaxed);
        let project_label = session.as_ref()
            .and_then(|value| config.projects.iter().find(|project| project.id == value.project_id))
            .map(|project| project.name.as_str()).unwrap_or("Local workspace");
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
                Command::Help => terminal.show_help(COMMANDS),
                Command::Exit => return Ok(()),
                Command::Init => {
                    (|| -> Result<()> {
                        let cwd = env::current_dir()?;
                        let project = config.select_working_directory(&cwd)?.clone();
                        config.save().context("saving Echo workspace selection")?;
                        session = Some(current_session(config, store)?);
                        show_session(config, terminal, session.as_ref());
                        if workspace::initialize(config, &project)? {
                            terminal.info(&format!("Amity is ready in {}. Choose /model if an AI model is not configured.",
                                project.path.display()));
                        } else {
                            if project.path.join(".amity").is_dir() {
                                terminal.info("Amity is already initialized for this folder.");
                            } else {
                                terminal.info("This project already uses an existing Amity connection. /init left it unchanged.");
                            }
                        }
                        Ok(())
                    })()
                }
                Command::Project => {
                    (|| -> Result<()> {
                        if choose_project(config, terminal)? {
                            session = Some(current_session(config, store)?);
                            show_session(config, terminal, session.as_ref());
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
                            show_session(config, terminal, session.as_ref());
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
                            show_session(config, terminal, session.as_ref());
                        } else {
                            terminal.info("Echo could not select the current working directory.");
                        }
                        Ok(())
                    })()
                }
                Command::Model => {
                    if let Some(current) = session.as_mut() {
                        choose_model(config, store, terminal, current)
                    } else {
                        terminal.info("Echo could not select the current working directory.");
                        Ok(())
                    }
                }
                Command::Attach => {
                    (|| -> Result<()> {
                        let Some(current) = session.as_mut() else {
                            terminal.info("Echo could not select the current working directory.");
                            return Ok(());
                        };
                        let Some(path) = terminal.prompt_text("Image or text file to attach")? else { return Ok(()); };
                        let reference = attachments::attach(path.trim().trim_matches('"'))?;
                        if current.pending_attachments.len() >= 2 {
                            bail!("attach at most two files to a message");
                        }
                        terminal.info(&format!("Attached {} to your next message.", reference.name));
                        current.pending_attachments.push(reference);
                        store.save(current)?;
                        Ok(())
                    })()
                }
                Command::Settings => choose_settings(config, terminal),
                Command::Status => {
                    if let Some(current) = session.as_mut() {
                        let mut agent = Agent { config, store, session: current, events: terminal,
                            cancel, interactive: true };
                        agent.status()
                    } else {
                        terminal.info("Echo is ready in this workspace.");
                        Ok(())
                    }
                }
                Command::ReviewChanges => {
                    if let Some(current) = session.as_mut() {
                        let mut agent = Agent { config, store, session: current, events: terminal,
                            cancel, interactive: true };
                        agent.review()
                    } else {
                        terminal.info("There are no changes to review in this workspace.");
                        Ok(())
                    }
                }
                Command::Cancel => {
                    if let Some(current) = session.as_mut() {
                        if matches!(&current.status, SessionStatus::Running | SessionStatus::WaitingApproval
                            | SessionStatus::AwaitingUserDecision | SessionStatus::Interrupted | SessionStatus::Failed)
                            && (current.active_task.is_some() || current.pending_policy_change.is_some()
                                || current.pending_capability.is_some()) {
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
            if matches!(command.id, Command::Init | Command::Project | Command::Sessions | Command::Settings) {
                steward.restart(config);
            }
            continue;
        }
        if let Some(current) = session.as_mut() {
            {
                let mut agent = Agent { config, store, session: current, events: terminal,
                    cancel, interactive: true };
                if let Err(error) = agent.instruction(line) { agent.events.error(&format!("{error:#}")); }
            }
            let latest = Config::load().context("reloading Echo capability settings")?;
            if latest.capabilities != config.capabilities {
                config.capabilities = latest.capabilities;
                steward.restart(config);
            }
        } else {
            terminal.info("Echo could not open a conversation for this working directory.");
        }
    }
}

struct StewardMonitor {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StewardMonitor {
    fn new(config: &Config) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = capabilities::persistent::start_steward_monitor(config, Arc::clone(&stop));
        Self { stop, handle }
    }

    fn restart(&mut self, config: &Config) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() { let _ = handle.join(); }
        *self = Self::new(config);
    }
}

impl Drop for StewardMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() { let _ = handle.join(); }
    }
}

fn show_session(config: &Config, terminal: &Terminal, session: Option<&Session>) {
    if let Some(session) = session {
        terminal.load_session(session);
        let name = config.projects.iter().find(|project| project.id == session.project_id)
            .map(|project| project.name.as_str()).unwrap_or(&session.project_id);
        terminal.context(name, &session.id, session.model.as_deref().unwrap_or("not configured"));
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
    options.push(MenuChoice { command: "Use an existing Amity connection".into(),
        description: "Enable governed coding in a configured workspace".into() });
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
    let project = active_project(config)?;
    if project.host_config.is_none() {
        terminal.info("This folder is already your Echo project. Run /init once to enable its Amity connection, then choose a model.");
        return Ok(());
    }
    let models = project.models.clone();
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
    options.push(MenuChoice { command: "Connect local Ollama model".into(),
        description: "Find installed models and connect one automatically".into() });
    options.push(MenuChoice { command: "Connect API model".into(),
        description: "Discover models from a Chat Completions compatible API".into() });
    options.push(MenuChoice { command: "Use an existing Amity model connection".into(),
        description: "Select a connection JSON file already configured for this project".into() });
    let Some(index) = terminal.choose_option("Choose an AI model", &options)? else { return Ok(()); };
    let mut next_config = config.clone();
    let selected = if index == options.len() - 3 {
        let endpoint = terminal.prompt_text("Ollama address (Enter for http://127.0.0.1:11434)")?
            .unwrap_or_default();
        let endpoint = if endpoint.trim().is_empty() { "http://127.0.0.1:11434" }
            else { endpoint.trim() };
        let names = bridge::discover_ollama(endpoint)?;
        if names.is_empty() { bail!("Ollama has no installed models. Install a model in Ollama first."); }
        let choices = names.iter().map(|name| MenuChoice {
            command:name.clone(), description:"Available in Ollama".into()
        }).collect::<Vec<_>>();
        let Some(chosen) = terminal.choose_option("Choose a local model", &choices)? else { return Ok(()); };
        let name = names[chosen].clone();
        let source = active_project(&next_config)?.host_config.clone()
            .context("This workspace has no Amity model connection")?;
        let mut binding: serde_json::Value = serde_json::from_slice(&std::fs::read(source)?)?;
        let fields = binding.as_object_mut().context("Amity connection is not a JSON object")?;
        fields.insert("model_command".into(), serde_json::json!(env::current_exe()?));
        fields.insert("model_args".into(), serde_json::json!([
            "__model_bridge", "ollama", endpoint, name
        ]));
        fields.insert("model_version".into(), serde_json::json!(name));
        let digest = format!("{:x}", Sha256::digest(format!("{}:{name}",
            active_project(&next_config)?.id).as_bytes()));
        let path = Config::home_dir().join("connections").join(format!("{digest}.json"));
        state::write_json_atomic(&path, &binding)?;
        next_config.active_project_mut().context("No project is selected")?
            .add_model(&name, &path)?;
        if let Some(profile) = next_config.active_project_mut()
            .and_then(|project| project.models.iter_mut().find(|profile| profile.name == name)) {
            profile.supports_images = bridge::ollama_supports_vision(endpoint, &name).unwrap_or(false);
        }
        Some(name)
    } else if index == options.len() - 2 {
        let endpoint = terminal.prompt_text("API base URL (for example https://api.openai.com/v1)")?
            .context("API setup cancelled")?;
        let key_env = terminal.prompt_text("Credential environment variable (Enter for AMITY_ECHO_API_KEY)")?
            .unwrap_or_default();
        let key_env = if key_env.trim().is_empty() { "AMITY_ECHO_API_KEY" }
            else { key_env.trim() };
        bridge::validate_key_env(key_env)?;
        let names = bridge::discover_api_models(endpoint.trim(), key_env)?;
        if names.is_empty() { bail!("This API returned no models."); }
        let choices = names.iter().map(|name| MenuChoice {
            command: name.clone(), description: "Available from this API".into()
        }).collect::<Vec<_>>();
        let Some(chosen) = terminal.choose_option("Choose an API model", &choices)? else { return Ok(()); };
        let model_id = names[chosen].clone();
        let digest = format!("{:x}", Sha256::digest(format!("{}:{}:{model_id}",
            active_project(&next_config)?.id, endpoint.trim()).as_bytes()));
        let profile_name = format!("API: {model_id} [{}]", &digest[..8]);
        let source = active_project(&next_config)?.host_config.clone()
            .context("This workspace has no Amity model connection")?;
        let mut binding: serde_json::Value = serde_json::from_slice(&std::fs::read(source)?)?;
        let fields = binding.as_object_mut().context("Amity connection is not a JSON object")?;
        fields.insert("model_command".into(), serde_json::json!(env::current_exe()?));
        fields.insert("model_args".into(), serde_json::json!([
            "__model_bridge", "api_chat", endpoint.trim(), model_id, key_env
        ]));
        fields.insert("model_version".into(), serde_json::json!(model_id));
        let path = Config::home_dir().join("connections").join(format!("{digest}.json"));
        state::write_json_atomic(&path, &binding)?;
        next_config.active_project_mut().context("No project is selected")?
            .add_model(&profile_name, &path)?;
        let image_choices = [
            MenuChoice { command: "Text only".into(),
                description: "Use when the API model does not accept images".into() },
            MenuChoice { command: "Images and text".into(),
                description: "Choose only if this provider documents image input for the model".into() },
        ];
        if let Some(choice) = terminal.choose_option("What input does this model support?", &image_choices)? {
            if let Some(profile) = next_config.active_project_mut()
                .and_then(|project| project.models.iter_mut()
                    .find(|profile| profile.name == profile_name)) {
                profile.supports_images = choice == 1;
            }
        }
        Some(profile_name)
    } else if index == options.len() - 1 {
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
            MenuChoice { command: "MCP connections".into(),
                description: "Connect or remove external development services".into() },
            MenuChoice { command: "Capabilities".into(),
                description: "Choose which engineering features Echo can use".into() },
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
            8 => changed = configure_mcp(&mut next, terminal)?,
            9 => changed = configure_capabilities(&mut next, terminal)?,
            _ => unreachable!(),
        }
        if !changed { continue; }
        next.save()?;
        *config = next;
        if index == 1 { terminal.set_color(config.terminal.color); }
        terminal.info("Setting saved.");
    }
}

fn configure_capabilities(config: &mut Config, terminal: &mut Terminal) -> Result<bool> {
    let project = config.active_project().cloned();
    let descriptors = capabilities::descriptors(config, project.as_ref(), None);
    let profile = config.capabilities.get("budget_profile").and_then(serde_json::Value::as_str)
        .unwrap_or("balanced");
    let retention = project.as_ref().is_some_and(|project| {
        let setting = config.capabilities.get("projects")
            .and_then(|projects| projects.get(&project.id))
            .and_then(|settings| settings.pointer("/overrides/experience_loop"));
        let enabled = setting.and_then(|setting| setting.get("enabled"))
            .and_then(serde_json::Value::as_bool).unwrap_or(false);
        let retained = setting.and_then(|setting| setting.get("retain"))
            .and_then(serde_json::Value::as_bool)
            .or_else(|| config.capabilities.pointer("/privacy/experience_retention")
                .and_then(serde_json::Value::as_bool)).unwrap_or(false);
        enabled && retained
    });
    let mut options = vec![
        MenuChoice { command: "Resource use".into(),
            description: format!("{} · limits model work and parallel workers", profile.replace('_', " ")) },
        MenuChoice { command: "Keep experience lessons".into(),
            description: if project.is_none() { "Connect a project first" }
                else if retention { "On for this project · local to this computer" }
                else { "Off for this project" }.into() },
    ];
    for descriptor in &descriptors {
        let auto_monitor = project.as_ref().and_then(|project| config.capabilities
            .get("projects").and_then(|value| value.get(&project.id)))
            .and_then(|value| value.pointer("/overrides/project_steward/auto_monitor"))
            .and_then(serde_json::Value::as_bool).unwrap_or(false);
        let status = if descriptor.id == "project_steward" && auto_monitor {
            "Scheduled while Echo is open".to_owned()
        } else if descriptor.activation_mode == "always" { "Always on".to_owned() }
            else if descriptor.availability.enabled {
                if descriptor.availability.activation == "manual" { "On request".into() }
                else { "Automatic when useful".into() }
            } else { "Off".into() };
        options.push(MenuChoice { command: descriptor.name.into(),
            description: format!("{status} · {}", descriptor.purpose) });
    }
    let Some(choice) = terminal.choose_option("Echo capabilities", &options)? else { return Ok(false); };
    if choice == 0 {
        let choices = [
            MenuChoice { command: "Low cost".into(), description: "Short investigations and one worker".into() },
            MenuChoice { command: "Balanced".into(), description: "Reasonable limits for normal development".into() },
            MenuChoice { command: "Extensive".into(), description: "Larger bounded investigations".into() },
        ];
        let Some(index) = terminal.choose_option("Resource use", &choices)? else { return Ok(false); };
        config.capabilities["budget_profile"] = serde_json::json!(match index {
            0 => "low_cost", 1 => "balanced", _ => "extensive",
        });
        return Ok(true);
    }
    if choice == 1 {
        let Some(project) = &project else {
            terminal.info("Echo could not select the current working directory.");
            return Ok(false);
        };
        let setting = &mut config.capabilities["projects"][project.id.as_str()]
            ["overrides"]["experience_loop"];
        setting["enabled"] = serde_json::json!(!retention);
        setting["retain"] = serde_json::json!(!retention);
        setting["activation"] = serde_json::json!(if retention { "off" } else { "manual" });
        return Ok(true);
    }
    let descriptor = &descriptors[choice - 2];
    if descriptor.activation_mode == "always" {
        terminal.info(&format!("{}: {}", descriptor.name, descriptor.purpose));
        terminal.info(&descriptor.availability.reason);
        return Ok(false);
    }
    let choices = if descriptor.id == "project_steward" {
        vec![
            MenuChoice { command: "Available on request".into(),
                description: "Inspect project health only when you ask".into() },
            MenuChoice { command: "Monitor while Echo is open".into(),
                description: "Run read-only checks on a schedule while this terminal is open".into() },
            MenuChoice { command: "Off".into(), description: "Do not monitor this project".into() },
        ]
    } else if descriptor.activation_mode == "conditional" {
        vec![
            MenuChoice { command: "Automatic when useful".into(),
                description: "Echo may use this for relevant work within your resource limits".into() },
            MenuChoice { command: "Only when I ask".into(),
                description: "Available for explicit requests".into() },
            MenuChoice { command: "Off".into(), description: "Do not activate".into() },
        ]
    } else {
        vec![
            MenuChoice { command: "Enable on request".into(),
                description: "Keep this feature available when you explicitly use it".into() },
            MenuChoice { command: "Off".into(), description: "Do not activate".into() },
        ]
    };
    let Some(index) = terminal.choose_option(descriptor.name, &choices)? else { return Ok(false); };
    let (enabled, activation) = if descriptor.id == "project_steward" {
        match index { 0 | 1 => (true, "manual"), _ => (false, "off") }
    } else if descriptor.activation_mode == "conditional" {
        match index { 0 => (true, "automatic"), 1 => (true, "manual"), _ => (false, "off") }
    } else if index == 0 { (true, "manual") } else { (false, "off") };
    let scope = if let Some(project) = &project {
        &mut config.capabilities["projects"][project.id.as_str()]["overrides"][descriptor.id]
    } else { &mut config.capabilities["overrides"][descriptor.id] };
    scope["enabled"] = serde_json::json!(enabled);
    scope["activation"] = serde_json::json!(activation);
    if descriptor.id == "experience_loop" {
        scope["retain"] = serde_json::json!(enabled);
    }
    if descriptor.id == "project_steward" {
        scope["auto_monitor"] = serde_json::json!(index == 1);
        if index == 1 {
            let interval = terminal.prompt_text("Check every how many minutes? (minimum 5)")?;
            if let Some(interval) = interval {
                let minutes = positive(&interval)?;
                if !(5..=43_200).contains(&minutes) {
                    bail!("Choose between 5 and 43,200 minutes.");
                }
                scope["interval_seconds"] = serde_json::json!(minutes * 60);
            } else { return Ok(false); }
        }
    }
    Ok(true)
}

fn configure_mcp(config: &mut Config, terminal: &mut Terminal) -> Result<bool> {
    let Some(project) = config.active_project().cloned() else {
        terminal.info("Connect a project before adding an MCP service.");
        return Ok(false);
    };
    let host_config = project.host_config.as_ref()
        .context("Amity is not configured for this workspace")?;
    let base: serde_json::Value = serde_json::from_slice(&std::fs::read(host_config)?)?;
    let servers = base.get("mcp_servers").and_then(serde_json::Value::as_array)
        .cloned().unwrap_or_default();
    let mut options = vec![MenuChoice { command:"Connect MCP service".into(),
        description:"Add an HTTPS MCP endpoint to this project".into() }];
    for server in &servers {
        if let Some(id) = server.get("id").and_then(serde_json::Value::as_str) {
            options.push(MenuChoice { command:format!("Remove {id}"),
                description:"Disconnect this MCP service".into() });
        }
    }
    let Some(choice) = terminal.choose_option("Project MCP services", &options)? else { return Ok(false); };
    let (remove, id, new_server) = if choice == 0 {
        let Some(id) = terminal.prompt_text("Name for this service")? else { return Ok(false); };
        let id = id.trim().to_owned();
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|byte|
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
            bail!("service name must use letters, numbers, hyphens, or underscores");
        }
        let Some(url) = terminal.prompt_text("HTTPS MCP endpoint URL")? else { return Ok(false); };
        if !url.starts_with("https://") || url.contains('@') || url.contains('#') {
            bail!("MCP service must use an HTTPS URL without embedded credentials");
        }
        let bearer_env = terminal.prompt_text("Bearer token environment variable (optional, AMITY_MCP_...)")?
            .filter(|value| !value.trim().is_empty());
        if bearer_env.as_deref().is_some_and(|name| !name.starts_with("AMITY_MCP_")
            || !name.bytes().all(|byte| byte.is_ascii_uppercase()
                || byte.is_ascii_digit() || byte == b'_')) {
            bail!("credential name must be an AMITY_MCP_ environment variable");
        }
        let server = serde_json::json!({"id":id,"url":url,"bearer_env":bearer_env});
        (false, id, Some(server))
    } else {
        let id = options[choice].command.strip_prefix("Remove ").unwrap_or("").to_owned();
        (true, id, None)
    };
    let project = config.active_project_mut().context("No project is selected")?;
    let mut bindings = vec![(None, project.host_config.clone()
        .context("Amity is not configured for this workspace")?)];
    bindings.extend(project.models.iter().enumerate()
        .map(|(index, profile)| (Some(index), profile.host_config.clone())));
    for (index, source) in bindings {
        let mut binding: serde_json::Value = serde_json::from_slice(&std::fs::read(source)?)?;
        let fields = binding.as_object_mut().context("Amity connection is not a JSON object")?;
        let mut current = fields.get("mcp_servers").and_then(serde_json::Value::as_array)
            .cloned().unwrap_or_default();
        current.retain(|server| server.get("id").and_then(serde_json::Value::as_str) != Some(&id));
        if !remove { current.push(new_server.clone().context("new server is missing")?); }
        fields.insert("mcp_servers".into(), serde_json::Value::Array(current));
        let label = index.map(|number| project.models[number].name.as_str()).unwrap_or("default");
        let digest = format!("{:x}", Sha256::digest(
            format!("{}:mcp:{label}",project.id).as_bytes()));
        let path = Config::home_dir().join("connections").join(format!("mcp-{digest}.json"));
        state::write_json_atomic(&path, &binding)?;
        if let Some(index) = index { project.models[index].host_config = path; }
        else { project.host_config = Some(path); }
    }
    terminal.info(if remove { "MCP service removed." } else { "MCP service saved. Echo will discover its tools when needed." });
    Ok(true)
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
echocli --json \"instruction\"    Stream machine-readable events\n\
echocli --attach <file> \"task\"  Include an image or text file\n\
echocli update [--check]        Install or check for an update\n\
echocli --version               Show the installed version\n\
echocli --help                  Show this help\n\nInside Echo:");
    for command in COMMANDS {
        println!("  {:<18} {}", command.name, command.description);
    }
}

