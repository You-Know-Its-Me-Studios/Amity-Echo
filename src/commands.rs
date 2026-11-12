//! The single public slash-command contract used by dispatch, help, menu, and history.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Model,
    Attach,
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
    CommandSpec { name: "/help", description: "Show available commands and how to use them.", id: Command::Help },
    CommandSpec { name: "/model", description: "Choose which AI model Echo uses.", id: Command::Model },
    CommandSpec { name: "/attach", description: "Attach an image or text file to your next message.", id: Command::Attach },
    CommandSpec { name: "/project", description: "Select or connect a project.", id: Command::Project },
    CommandSpec { name: "/sessions", description: "Browse and reopen previous conversations.", id: Command::Sessions },
    CommandSpec { name: "/new-session", description: "Start a new conversation in this project.", id: Command::NewSession },
    CommandSpec { name: "/status", description: "See what Echo is currently doing.", id: Command::Status },
    CommandSpec { name: "/settings", description: "View or change Echo's settings.", id: Command::Settings },
    CommandSpec { name: "/review-changes", description: "Review a proposed change and decide whether to apply it.", id: Command::ReviewChanges },
    CommandSpec { name: "/cancel", description: "Cancel the current operation.", id: Command::Cancel },
    CommandSpec { name: "/exit", description: "Close Echo and return to the terminal.", id: Command::Exit },
];

pub fn find(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|command| command.name == name)
}
