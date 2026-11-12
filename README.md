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
echocli --json "Summarize this project's Git changes"
echocli --attach .\mockup.png "Implement this layout"
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
| `/attach` | Add an image or text file to your next message. |

Echo presents approvals and policy decisions when they are needed. You can describe coding work in ordinary language; Echo coordinates the agent and Amity workflow. If a project, model, or permission is missing, Echo explains the next step. Press Ctrl+C at the idle composer to return to the shell. During active work, Ctrl+C cancels the current operation.

The conversation browser shows sessions from the current working directory by default. Tab switches between that directory and all registered projects, F2 changes sort order, F3 filters by session status, Enter opens the selected conversation, and Esc returns without switching. Reopening a conversation restores its stored messages. New sessions record their starting directory; older sessions without that field appear under their registered project root.

Echo stores configuration and recoverable session JSON under `%LOCALAPPDATA%\Amity\Echo` on Windows, or under the platform home/config directory elsewhere. `AMITY_ECHO_HOME` overrides that location. Amity remains the source of truth for project context, permissions, tasks, and evidence. Echo redacts common credential forms before saving conversations; avoid placing secrets in development instructions.

## Models, tools, and automation

Use `/model` to connect an installed Ollama model, discover models from a Chat Completions compatible API, or select an existing Amity model connection. For an API connection, set the credential in the environment before launching Echo, for example `$env:AMITY_ECHO_API_KEY = 'your-key'` in PowerShell. Echo stores the API URL, model ID, and credential variable name in its local connection file; the key value is never saved there. `/model` discovers model IDs from the provider's `/models` endpoint. Ollama vision support is detected from its model metadata. API model image support depends on the selected provider and is not automatically inferred from the model name.

The selected model can use Amity-governed file search, reads, proposals, configured commands, Git status/diff/stage/commit/push, checkpoints, and undo. Ask conversationally to inspect or restore a checkpoint. Git diff requires exact file paths; stage and commit require scoped files and human approval. Repositories with executable Git content filters must stage through an Amity-approved project command. Undo refuses to overwrite a file changed since its checkpoint and submits the inverse change through Amity's policy and approval boundary. Commands, edits, Git writes, and MCP calls remain subject to Amity's active project contract.

Echo compacts older conversation turns into a stored model-written summary while retaining the original session messages. It can run up to four read-only model workers for independent investigations; their findings return to the parent agent, which coordinates all writes. Put reusable `SKILL.md` files in `%LOCALAPPDATA%\Amity\Echo\skills\<name>\SKILL.md` (or under `AMITY_ECHO_HOME`). Skill text is treated as guidance and cannot grant permissions.

Use `/settings` → **MCP connections** to add an HTTPS Streamable HTTP MCP service. A bearer token, if required, must be supplied through the named `AMITY_MCP_*` environment variable. Tool discovery is read-only; every MCP tool call asks for human approval and is checked by Amity. Echo does not execute local stdio MCP servers or treat a remote service as a project filesystem executor.

`--attach <file>` and `/attach` accept explicit user-selected images and common text/source files up to 4 MiB. Session history stores their paths and digests, not their bytes. The file must still match its digest when sent. Image delivery requires a vision-capable model connection. The optional VS Code adapter in `editor/vscode` sends the active file, selection, and diagnostics to a one-shot Echo request; editor context is validated against the connected project and is not saved in the session. Install the `amity-echo-editor.vsix` asset from the matching GitHub release with VS Code's **Install from VSIX** command.

For scripts, `echocli --json "instruction"` emits one JSON event per line and a final result with session ID, task ID, status, and reported token usage. Exit codes are 0 for completion, 1 for failure, 2 for pending approval, 3 for a policy decision, and 4 for interruption or cancellation. `--editor-context <json-file>` supplies a trusted editor adapter payload. During interactive work, you can type a new instruction while the model is responding; Echo queues it for the next agent turn. Ctrl+C cancels the current operation.

## Updates

Windows x64 releases are published on the public [Amity Echo GitHub Releases](https://github.com/You-Know-Its-Me-Studios/Amity-Echo/releases) page. The interactive top bar shows `echocli update` when a newer Windows release is available. A failed update check does not interrupt input or display an error. `echocli --version` shows the installed version.

Run `echocli update` from any directory to download the latest release, verify its size and SHA-256 checksum, check GitHub's release digest when supplied, and replace the installed executable. Restart Echo to use the new version. Development binaries under `target` must be installed through Cargo. No GitHub account or GitHub CLI is needed for versions 0.1.2 and later. Echo's local configuration and sessions are separate from the executable.

For maintainers, set matching versions in `Cargo.toml` and `editor/vscode/package.json`, commit them on `main`, then push a matching `v<version>` tag pointing to that commit. The tag starts the Windows release workflow. It builds `echocli-x86_64-pc-windows-msvc.exe` from the tracked `Cargo.lock` and publishes that asset with its `.sha256` file and the VS Code `.vsix`. A source commit alone does not publish an update. The matching Amity backend must also be installed for the new governed Git, checkpoint, and MCP operations.
