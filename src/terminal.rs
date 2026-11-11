//! Human-facing terminal presentation. The agent and host modules own decisions;
//! this module only collects input and renders their state.

use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Line(String),
    Interrupt,
    Eof,
}

pub struct Terminal {
    editor: Option<DefaultEditor>,
    history_path: PathBuf,
    color: bool,
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
        })
    }

    pub fn read_line(&mut self, project: &str, session: &str) -> Result<Input> {
        let prompt = format!(
            "{} {} {} ",
            self.paint("Echo", "1;36"),
            self.paint(&format!("[{project}]"), "36"),
            self.paint(&format!("({session}) ›"), "2")
        );
        self.read_with_prompt(&prompt, true)
    }

    pub fn ask_approval(&mut self, description: &str) -> Result<bool> {
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
        println!("{}", self.paint("  Amity Echo", "1;36"));
        println!(
            "  {}  {}  {}",
            self.paint(&format!("Project: {project}"), "36"),
            self.paint(&format!("Session: {session}"), "2"),
            self.paint(&format!("Model: {model}"), "2")
        );
        println!("  Type /help for commands. Ctrl+C interrupts input; Ctrl+D exits.\n");
    }

    pub fn context(&self, project: &str, session: &str, model: &str) {
        println!(
            "{} {} · {} · {}",
            self.paint("Echo", "1;36"),
            Self::clean(project),
            Self::clean(session),
            Self::clean(model)
        );
    }

    pub fn assistant(&self, message: &str) {
        println!("\n{}", self.paint("Echo", "1;36"));
        self.render_markdown(&Self::clean(&crate::state::redact_credentials(message)));
        println!();
    }

    pub fn info(&self, message: &str) {
        println!("{} {}", self.paint("•", "36"), Self::clean(&crate::state::redact_credentials(message)));
    }

    pub fn warning(&self, message: &str) {
        eprintln!("{} {}", self.paint("!", "1;33"), Self::clean(&crate::state::redact_credentials(message)));
    }

    pub fn error(&self, message: &str) {
        eprintln!("{} {}", self.paint("Error:", "1;31"), Self::clean(&crate::state::redact_credentials(message)));
    }

    pub fn operation(&self, message: &str) {
        println!("{} {}", self.paint("↳", "1;34"), Self::clean(&crate::state::redact_credentials(message)));
        let _ = io::stdout().flush();
    }

    pub fn tool_result(&self, name: &str, result: &str, success: bool) {
        let icon = if success { "✓" } else { "×" };
        let style = if success { "32" } else { "31" };
        println!("{} {}", self.paint(icon, style), self.paint(name, "1"));
        let safe = crate::state::redact_credentials(result);
        for line in safe.lines() {
            println!("  {}", self.paint(&Self::clean(line), "2"));
        }
    }

    pub fn proposal(&self, title: &str, details: &str) {
        println!("\n{} {}", self.paint("Proposal:", "1;33"), Self::clean(title));
        self.render_markdown(&Self::clean(&crate::state::redact_credentials(details)));
        println!();
    }

    pub fn raw(&self, value: &str) {
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
