# Amity Echo

Amity Echo is a local Rust coding agent launched as `echocli`. It keeps its own conversations and delegates project context, ACL decisions, task orchestration, gateway changes, approvals, and engineering evidence to the existing Amity host.

## Setup

Echo needs an initialized Amity project, its existing host JSON configuration, an `amity` host executable, and `AMITY_HOST_TOKEN` in the calling environment. The token is passed to the host process and is not saved in Echo configuration. The selected Amity host config must contain a configured `model_command`, `model_version`, and authorized `model_actor_id` for agent work. The model command follows Amity's `ModelInput` JSON on stdin and `ModelTurn` JSON on stdout contract. Echo does not provide an inference runtime or provision model membership.

Install the Rust binary from this directory with `cargo install --path . --force`; this creates the `echocli` command. Installations that predate the update command need this one-time reinstall.

## Updates

Windows x64 releases are published to the public [Amity Echo GitHub Releases](https://github.com/You-Know-Its-Me-Studios/Amity-Echo/releases) page. `echocli --version` shows the installed version, and `echocli update --check` reports the latest version and release link without installing it. Run `echocli update` from any directory to download the latest release, verify its size and SHA-256 checksum, check GitHub's release digest when supplied, then replace the installed executable. Restart `echocli` to use the new version. Development binaries under `target` must be installed through Cargo instead. No GitHub account or GitHub CLI is needed for version 0.1.2 and later. Echo's local configuration and sessions are separate from the executable.

Version 0.1.1 still uses the GitHub CLI for its own update command. To move from 0.1.1 to 0.1.2, use that command with an authenticated `gh`, or reinstall once from this checkout with `cargo install --path . --force`.

For maintainers, set the new version in `Cargo.toml`, commit it on `main`, then push a matching `v<version>` tag pointing to that commit. The tag starts the Windows release workflow. It builds `echocli-x86_64-pc-windows-msvc.exe` from the tracked `Cargo.lock` and publishes that asset with `echocli-x86_64-pc-windows-msvc.exe.sha256` in the same release. A source commit alone does not publish an update.

`echocli` opens Echo's full-screen terminal from any directory, including before a project is connected. The composer remains available; `/project add <amity-host.json>` and `/config host <amity-executable>` connect the existing Amity infrastructure. Press Ctrl+C at the idle composer to return to the shell. During model or host work, Ctrl+C cancels the active operation. Project instructions need a connected, authorized Amity project and a configured model.

```powershell
echocli config host "C:\path\to\amity.exe"
echocli project add "C:\path\to\amity-host.json"
echocli
```

Set `AMITY_HOST_CONFIG` to an existing host JSON to register its bound project on first launch. Project selection later follows the current working directory when it is inside a registered project; elsewhere Echo retains the selected project and displays the working directory. `echocli project use <id>` switches explicitly.

Model profiles point at other preconfigured Amity host JSON files for the same authorized project root and ID:

```powershell
echocli model add local "C:\path\to\amity-local-model.json"
echocli model use local
```

The Amity host remains responsible for authorization and model grants. Selecting a profile cannot grant access. A Lyra profile can be added when a compatible inference backend exists.

## Use

```powershell
echocli
echocli "Investigate why saving fails in the inventory system."
echocli task "Fix the inventory save failure."
echocli task-steps .\task-plan.json
echocli sessions
echocli resume
echocli status
echocli --version
echocli update --check
echocli review
echocli approve --yes
```

Interactive commands include `/task`, `/task-steps`, `/status`, `/review`, `/approve`, `/continue`, `/interrupt`, `/retry`, `/cancel`, `/reconcile`, `/report`, `/raw`, `/tools`, `/session new`, `/resume`, `/project`, `/model`, and `/exit`. Reopening or switching a session restores its stored conversation in the terminal. `echocli --help` gives the complete command list. Direct instructions are suitable for scripting; proposals remain pending until an explicit approval command. `echocli task` forces the host task workflow when a natural instruction is ambiguous. A `task-steps` file contains Amity's existing `{ "request": "...", "steps": [{ "id": "...", "instruction": "...", "depends_on": [] }] }` task shape.

Type `/` in the interactive composer to open Echo's command menu. The list filters as you type; use Up and Down to choose, Tab to complete, Enter to select, and Esc to dismiss it without clearing your draft. `/model`, `/project`, and `/resume` open choices from configured model profiles, registered projects, and stored sessions. Selecting one runs the corresponding Echo command through its existing authorization and session flow.

Echo stores configuration and recoverable session JSON under `%LOCALAPPDATA%\Amity\Echo` on Windows, or under the platform home/config directory elsewhere. `AMITY_ECHO_HOME` overrides that location. Session files contain Echo conversation text and links to Amity task IDs; Amity retains authoritative project context, permissions, task transcripts, and evidence. Echo redacts common credential forms before persistence. Avoid placing secrets in development instructions.

The selected model process powers conversational answers using Amity context and read-only AIP queries. Coding changes run through Amity's `task-create`, `task-run`, AIP proposal, gateway approval and execution, and task report commands. Echo surfaces host or model failures rather than reporting a change it cannot observe. A model adapter that only emits task gateway references cannot answer conversational questions; it must also implement Echo's conversational `ModelTurn` instruction contract. No remote service is included.
