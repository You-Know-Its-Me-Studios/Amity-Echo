# Amity Echo

Amity Echo is a local coding agent launched with `echocli`. It keeps conversations on your computer and uses the existing Amity host for project context, permissions, code changes, and engineering evidence.

## Install and connect

Install the Rust binary from this directory with `cargo install --path . --force`. This adds `echocli` to your terminal. Installations made before Echo gained its update command need this one-time reinstall.

Run `echocli` from any directory. Echo opens even when no project is connected. Use `/project` to select a connected project or connect one using its existing Amity host JSON configuration. Use `/settings` to set the Amity host executable if Echo cannot find it. The connection must point to an initialized and authorized Amity project. The selected host configuration needs a working `model_command`, `model_version`, and authorized `model_actor_id` for agent work. Echo does not provide its own model inference runtime or grant model permissions.

If `AMITY_HOST_CONFIG` points to an existing host JSON file, Echo can register its bound project on first launch. When you open Echo inside a registered project, it selects that project automatically. Elsewhere it retains your selected project and shows the current working directory. `AMITY_HOST_TOKEN` is read from the environment and passed to the host; Echo does not save it in its configuration.

## Use Echo

```powershell
echocli
echocli "Investigate why saving fails in the inventory system."
echocli resume
echocli sessions
echocli status
echocli --help
echocli --version
echocli update
```

`echocli` opens the full-screen terminal. A quoted instruction runs once and is suitable for scripting. `echocli resume` and `echocli sessions` open the searchable conversation browser; `echocli status` shows current work. `echocli update --check` checks for a newer release without installing it.

Inside Echo, type `/` to open the command menu. The menu filters as you type and describes each command. Use Up and Down to select, Tab to complete, Enter to choose, and Esc to dismiss it without clearing your draft.

| Command | What it does |
| --- | --- |
| `/help` | Show available commands and how to use them. |
| `/model` | Choose which AI model Echo uses. |
| `/project` | Select or connect a project. |
| `/sessions` | Browse and reopen previous conversations. |
| `/new-session` | Start a new conversation in this project. |
| `/status` | See what Echo is currently doing. |
| `/settings` | View or change Echo's settings. |
| `/review-changes` | Review a proposed change and decide whether to apply it. |
| `/cancel` | Cancel the current operation. |
| `/exit` | Close Echo and return to the terminal. |

Echo presents approvals and policy decisions when they are needed. You can describe coding work in ordinary language; Echo coordinates the agent and Amity workflow. If a project, model, or permission is missing, Echo explains the next step. Press Ctrl+C at the idle composer to return to the shell. During active work, Ctrl+C cancels the current operation.

The conversation browser shows sessions from the current working directory by default. Tab switches between that directory and all registered projects, F2 changes sort order, F3 filters by session status, Enter opens the selected conversation, and Esc returns without switching. Reopening a conversation restores its stored messages. New sessions record their starting directory; older sessions without that field appear under their registered project root.

Echo stores configuration and recoverable session JSON under `%LOCALAPPDATA%\Amity\Echo` on Windows, or under the platform home/config directory elsewhere. `AMITY_ECHO_HOME` overrides that location. Amity remains the source of truth for project context, permissions, tasks, and evidence. Echo redacts common credential forms before saving conversations; avoid placing secrets in development instructions.

## Updates

Windows x64 releases are published on the public [Amity Echo GitHub Releases](https://github.com/You-Know-Its-Me-Studios/Amity-Echo/releases) page. The interactive top bar shows `echocli update` when a newer Windows release is available. A failed update check does not interrupt input or display an error. `echocli --version` shows the installed version.

Run `echocli update` from any directory to download the latest release, verify its size and SHA-256 checksum, check GitHub's release digest when supplied, and replace the installed executable. Restart Echo to use the new version. Development binaries under `target` must be installed through Cargo. No GitHub account or GitHub CLI is needed for versions 0.1.2 and later. Echo's local configuration and sessions are separate from the executable.

For maintainers, set the new version in `Cargo.toml`, commit it on `main`, then push a matching `v<version>` tag pointing to that commit. The tag starts the Windows release workflow. It builds `echocli-x86_64-pc-windows-msvc.exe` from the tracked `Cargo.lock` and publishes that asset with its `.sha256` file. A source commit alone does not publish an update.
