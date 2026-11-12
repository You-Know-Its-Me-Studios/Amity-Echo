//! The single public slash-command contract used by dispatch, help, menu, and history.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Model,
    Project,
    Sessions,
    NewSession,
    Status,
    Settings,
    ReviewChanges,
    Cancel,
    Exit,
}

pub struct CommandSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub id: Command,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec { name: "/help", description: "Show commands and what they do", id: Command::Help },
    CommandSpec { name: "/model", description: "Choose the AI model for this conversation", id: Command::Model },
    CommandSpec { name: "/project", description: "Switch or connect a project", id: Command::Project },
    CommandSpec { name: "/sessions", description: "Browse and reopen conversations", id: Command::Sessions },
    CommandSpec { name: "/new-session", description: "Start a new conversation", id: Command::NewSession },
    CommandSpec { name: "/status", description: "See what Echo is doing", id: Command::Status },
    CommandSpec { name: "/settings", description: "View or change Echo settings", id: Command::Settings },
    CommandSpec { name: "/review-changes", description: "Review a proposed change", id: Command::ReviewChanges },
    CommandSpec { name: "/cancel", description: "Cancel the current operation", id: Command::Cancel },
    CommandSpec { name: "/exit", description: "Close Echo", id: Command::Exit },
];

pub fn find(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|command| command.name == name)
}
