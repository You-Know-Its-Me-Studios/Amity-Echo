# Amity Echo

Amity Echo is a local Rust coding agent launched as `echocli`. It keeps its own conversations and delegates project context, ACL decisions, task orchestration, gateway changes, approvals, and engineering evidence to the existing Amity host.

## Setup

Echo needs an initialized Amity project, its existing host JSON configuration, an `amity` host executable, and `AMITY_HOST_TOKEN` in the calling environment. The token is passed to the host process and is not saved in Echo configuration. The selected Amity host config must contain a configured `model_command`, `model_version`, and authorized `model_actor_id` for agent work. The model command follows Amity's `ModelInput` JSON on stdin and `ModelTurn` JSON on stdout contract. Echo does not provide an inference runtime or provision model membership.

Install the Rust binary from this directory with `cargo install --path .`; this creates the `echocli` command. The installation command is documentation only and was not run as part of this implementation.

```powershell
echocli config host "C:\path\to\amity.exe"
echocli project add "C:\path\to\project" "C:\path\to\amity-host.json"
echocli
```

Set `AMITY_HOST_CONFIG` to an existing host JSON to register the current directory on first launch. Project selection later follows the current working directory when it is inside a registered project. `echocli project use <id>` switches explicitly.

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
echocli review
echocli approve --yes
```

Interactive commands include `/task`, `/task-steps`, `/status`, `/review`, `/approve`, `/continue`, `/interrupt`, `/retry`, `/cancel`, `/reconcile`, `/report`, `/raw`, `/tools`, `/session new`, `/resume`, `/project`, `/model`, and `/exit`. `echocli --help` gives the complete command list. Direct instructions are suitable for scripting; proposals remain pending until an explicit approval command. `echocli task` forces the host task workflow when a natural instruction is ambiguous. A `task-steps` file contains Amity's existing `{ "request": "...", "steps": [{ "id": "...", "instruction": "...", "depends_on": [] }] }` task shape.

Echo stores configuration and recoverable session JSON under `%LOCALAPPDATA%\Amity\Echo` on Windows, or under the platform home/config directory elsewhere. `AMITY_ECHO_HOME` overrides that location. Session files contain Echo conversation text and links to Amity task IDs; Amity retains authoritative project context, permissions, task transcripts, and evidence. Echo redacts common credential forms before persistence. Avoid placing secrets in development instructions.

The selected model process powers conversational answers using Amity context and read-only AIP queries. Coding changes run through Amity's `task-create`, `task-run`, AIP proposal, gateway approval and execution, and task report commands. Echo surfaces host or model failures rather than reporting a change it cannot observe. A model adapter that only emits task gateway references cannot answer conversational questions; it must also implement Echo's conversational `ModelTurn` instruction contract. No remote service is included.
