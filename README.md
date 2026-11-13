# Amity Echo

Amity Echo is a local coding agent launched with `echocli`. It keeps conversations on your computer and uses the existing Amity host for project context, permissions, code changes, and engineering evidence.

## Install and initialize

Install the Rust binary from this directory with `cargo install --path . --force`. This adds `echocli` to your terminal. Installations made before Echo gained its update command need this one-time reinstall.

Run `echocli` from any directory. That directory is immediately the active Echo project, with its own persistent conversations. Run `/init` once to create its `.amity` directory and initialize Amity's active ACL, authority, gateway, intelligence index, task records, and evidence stores. The initial ACL requires owner approval for file and Git mutations, protects `.amity`, and prevents automatic tests. The authentication token is kept in Echo's local private storage outside the project. Use `/model` to select an AI model. Echo does not provide its own model inference runtime.

If `AMITY_HOST_CONFIG` points to an existing host JSON file, Echo can register its bound project on first launch. When you open Echo inside a registered project, it selects that project automatically. Otherwise, it registers the current directory as a local Echo project. `/project` remains available for switching projects or using an existing Amity connection. Existing external Amity connections continue to read `AMITY_HOST_TOKEN` from the environment; `/init` creates a separate local credential outside ordinary configuration.

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
| `/init` | Initialize Amity for the current project directory. |
| `/model` | Choose which AI model Echo uses. |
| `/project` | Switch workspaces or use an existing Amity connection. |
| `/sessions` | Browse and reopen previous conversations. |
| `/new-session` | Start a new conversation in this project. |
| `/status` | See what Echo is currently doing. |
| `/settings` | View or change Echo's settings. |
| `/review-changes` | Review a proposed change and decide whether to apply it. |
| `/cancel` | Cancel the current operation. |
| `/exit` | Close Echo and return to the terminal. |
| `/attach` | Add an image or text file to your next message. |

Echo presents approvals and policy decisions when they are needed. You can describe coding work in ordinary language; Echo coordinates the agent and Amity workflow. If the Amity host, model, or permission is missing, Echo explains the next step. Press Ctrl+C at the idle composer to return to the shell. During active work, Ctrl+C cancels the current operation.

The conversation browser shows sessions from the current working directory by default. Tab switches between that directory and all registered projects, F2 changes sort order, F3 filters by session status, Enter opens the selected conversation, and Esc returns without switching. Reopening a conversation restores its stored messages. New sessions record their starting directory; older sessions without that field appear under their registered project root.

Echo stores configuration and recoverable session JSON under `%LOCALAPPDATA%\Amity\Echo` on Windows, or under the platform home/config directory elsewhere. `AMITY_ECHO_HOME` overrides that location. Amity remains the source of truth for project context, permissions, tasks, and evidence. Echo redacts common credential forms before saving conversations; avoid placing secrets in development instructions.

## Models, tools, and automation

During local development, Echo connects to the existing **Cadenza/1 WebSocket API** at the global default `ws://127.0.0.1:7941/cadenza`. Start Cadenza separately when you want to use it. Echo never searches for or starts a Cadenza executable. `/settings` → **Cadenza connection** lets you change the URL and select an environment or Windows Credential Manager reference for the Cadenza/1 credential. The default reference is `env:CADENZA_API_TOKEN`, matching Cadenza's development configuration. The server and Echo must receive the same credential; Echo never stores its value in ordinary configuration. Remote URLs require certificate-verified `wss://`.

`/model` opens a compact two-column picker with dynamically discovered model names and readable descriptions. The highlighted row spans the terminal width, and `(current)` identifies the active model. Use arrows or Page Up/Down to browse, Enter to select, and Escape to cancel. Narrow terminals place descriptions below each name. A second reasoning-level screen is available only when the inference adapter can supply and apply supported effort choices; Cadenza/1 currently has no configurable reasoning control. Selection is pinned to a discovered root and saved globally, independently of the project directory. New sessions inherit the default. Existing conversations retain their identity and messages; selecting another model records the change for subsequent turns. Connection failures and unavailable models preserve the previous selection.

Echo attempts to connect at interactive startup. A stopped server produces `Cadenza unavailable` and leaves the terminal usable; a missing or rejected credential produces a separate error. A new turn reconnects and validates the selected model; interrupted output is never automatically replayed. Cadenza owns execution while Echo retains tools, permissions and Amity governance. Current Lyra is a foundation model: ordinary output, including JSON-looking text, remains text and cannot execute tools. Structured tool use requires explicitly advertised support and the existing permission checks. There is no alternative inference provider or compatible-API fallback. The future customer topology will route authenticated inference through Amity; this release does not implement customer account services. See [native integration and acceptance](docs/cadenza.md).

Migration backs up the previous global configuration as `config.pre-cadenza.json`, removes retired per-project model settings from active configuration, and preserves registration and the selected governance binding. Existing conversations remain readable. Open `/model` once to explicitly associate a previous-provider conversation with Cadenza before continuing it. Existing host files and historical conversation messages are not deleted.

The selected model can use Amity-governed file search, reads, proposals, configured commands, Git status/diff/stage/commit/push, checkpoints, and undo. Ask conversationally to inspect or restore a checkpoint. Git diff requires exact file paths; stage and commit require scoped files and human approval. Repositories with executable Git content filters must stage through an Amity-approved project command. Undo refuses to overwrite a file changed since its checkpoint and submits the inverse change through Amity's policy and approval boundary. Commands, edits, Git writes, and MCP calls remain subject to Amity's active project contract.

Echo compacts older conversation turns into a stored model-written summary while retaining the original session messages. It can run up to four read-only model workers for independent investigations; their findings return to the parent agent, which coordinates all writes. Put reusable `SKILL.md` files in `%LOCALAPPDATA%\Amity\Echo\skills\<name>\SKILL.md` (or under `AMITY_ECHO_HOME`). Skill text is treated as guidance and cannot grant permissions.

## Native engineering capabilities

Echo exposes one provider-independent model tool, `echo_capability`, using `echo.capabilities.v1`. The model can discover the live catalog and request a capability action; Echo checks availability, resource limits, consent, and session state. Any project write, executable command, Git change, editor launch, or other governed action still goes through Amity and its active ACL. Capabilities do not grant permissions. `/settings` → **Capabilities** controls project activation, resource use, local experience retention, and foreground project monitoring. No separate slash commands are needed.

The always available foundation covers Outcome Engine, Architecture Twin, Evidence Ledger, Reversible Engineering, Engineering Memory, and Black Box. Conditional capabilities cover Reality Loop, Solution Arena, Futurecast, Debugging Laboratory, Intelligent Workforce, and Visual-to-Code Navigation. Project Steward, Trust Certification, and Experience Loop require explicit project opt-in or user approval for their persistent or costly work. Capability state is stored with Echo sessions or project-scoped local records; Amity remains authoritative for project intelligence, policy, execution, verification, and checkpoints.

Reality Loop can inspect Amity task activity and request a configured runtime command or supported Unreal, Unity, or Godot editor query through the governed gateway. It does not provide general interactive playtesting. Solution Arena can compare read-only workers and materialize bounded candidate file copies outside the production project; those copies are not automatically executed or verified. Project Steward schedules read-only checks only while Echo is open. Trust Certification can inspect recorded behavior or run explicitly approved synthetic, read-only model protocol scenarios; neither grants a safety certification. Experience lessons stay local unless a user separately approves selected sanitized records for a local training export. Echo does not train a model or upload those records.

The versioned capability descriptors, input and output contracts, availability, positive and negative behavior, and training export rules are returned by `echo_capability` discovery. A model requests a capability with `{"protocol":"echo.capabilities.v1","capability":"architecture_twin","action":"observe","input":{"query":"inventory storage"}}`. Discovery with `{"protocol":"echo.capabilities.v1","action":"discover","input":{"include_training_contract":true}}` returns the machine-readable training contract. Inference uses Cadenza/1. The Amity host callback also uses this native client and cannot invoke stored legacy model commands.

Use `/settings` → **MCP connections** to add an HTTPS Streamable HTTP MCP service. A bearer token, if required, must be supplied through the named `AMITY_MCP_*` environment variable. Tool discovery is read-only; every MCP tool call asks for human approval and is checked by Amity. Echo does not execute local stdio MCP servers or treat a remote service as a project filesystem executor.

`--attach <file>` and `/attach` accept explicit user-selected images and common text/source files up to 4 MiB. Session history stores their paths and digests, not their bytes. The file must still match its digest when sent. Cadenza/1 has no image input transport, so image delivery fails explicitly while historical references remain readable. The optional VS Code adapter in `editor/vscode` sends the active file, selection, and diagnostics to a one-shot Echo request; editor context is validated against the connected project and is not saved in the session. Install the `amity-echo-editor.vsix` asset from the matching GitHub release with VS Code's **Install from VSIX** command.

For scripts, `echocli --json "instruction"` emits one JSON event per line and a final result with session ID, task ID, status, and reported token usage. Exit codes are 0 for completion, 1 for failure, 2 for pending approval, 3 for a policy decision, and 4 for interruption or cancellation. `--editor-context <json-file>` supplies a trusted editor adapter payload. During interactive work, you can type a new instruction while the model is responding; Echo queues it for the next agent turn. Ctrl+C cancels the current operation.

## Updates

Windows x64 releases are published on the public [Amity Echo GitHub Releases](https://github.com/You-Know-Its-Me-Studios/Amity-Echo/releases) page. The interactive top bar shows `echocli update` when a newer Windows release is available. A failed update check does not interrupt input or display an error. `echocli --version` shows the installed version.

Run `echocli update` from any directory to download the latest release, verify its size and SHA-256 checksum, check GitHub's release digest when supplied, and replace the installed executable. Releases from 0.3.4 onward also include the matching Amity host binary, which Echo installs under its local application directory; `/init` can fetch that verified host automatically if it is missing. Restart Echo to use the new version. Development binaries under `target` must be installed through Cargo. No GitHub account or GitHub CLI is needed. Echo's local configuration and sessions are separate from the executable.

For maintainers, set matching versions in `Cargo.toml` and `editor/vscode/package.json`, commit them on `main`, then push a matching `v<version>` tag pointing to that commit. The tag starts the Windows release workflow. It builds the Echo CLI and packages the pinned Amity host binary from Echo's pre-release host asset, then publishes both executables with `.sha256` files and the VS Code `.vsix`. The Amity host package is built in the private backend repository and updated separately when that backend changes. A source commit alone does not publish an update.
