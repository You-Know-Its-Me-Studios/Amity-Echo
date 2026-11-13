# Native Cadenza integration

Echo's inference backend is the proprietary authenticated Cadenza/1 WebSocket service. There is no provider discovery, generic compatible API, model HTTP bridge or alternative inference runtime. Cadenza executes models; Echo owns conversations, context, tools, permissions and orchestration. Cadenza, Lyra and the model-format repository were not modified.

## Echo inference contract, version 1

`src/inference.rs` is Echo's transport boundary. Its connection interface exposes `discover`, `health`, `validate_selection`, and `generate` to the model picker and agent. The current adapter is `src/cadenza.rs`, which speaks **Cadenza/1** exactly; this contract does not define another wire protocol. A future authenticated Amity relay must implement the same Echo-side behavior after its real server contract exists, without changing conversation persistence, the picker, or agent orchestration. The current selection contains the Cadenza identity and cryptographic model root. Historical selections remain stored with sessions when models change.

Connection establishes a bounded WebSocket on a loopback `ws://` or certificate-verified `wss://` endpoint, sends `hello` with version 1 and a credential obtained outside the conversation, and requires a version-1 acknowledgment. `discover` returns the service's actual models, identity, root, readiness, context limit, and reported capabilities; `status` confirms worker availability. Before inference, Echo revalidates the selected identity and root. `generate` supplies that exact root, a bounded UTF-8 prompt, and a requested output limit. `accepted` gives prompt usage, `token` carries an indexed **cumulative text snapshot**, and `complete` gives final usage and termination reason. Echo replaces its displayed snapshot rather than appending snapshots. Cancellation sends `cancel` for the active request and requires the service's terminal response; disconnect cancels owned work. A failed or interrupted stream is never silently replayed. Each new turn opens a new connection and can recover after a service restart. Structured errors remain distinct from missing service, missing credential, unavailable model, and interrupted transport.

## Configuration and migration

Connection settings live in the existing global `%LOCALAPPDATA%\Amity\Echo\config.json` (`AMITY_ECHO_HOME` override), under `cadenza`:

```json
{
  "endpoint": "ws://127.0.0.1:7941/cadenza",
  "credential_ref": "env:CADENZA_API_TOKEN",
  "default_model": null,
  "connect_timeout_seconds": 5,
  "request_timeout_seconds": 120,
  "max_tokens": 32
}
```

`default_model`, when selected, records the actual discovered identity, cryptographic root and discovery metadata. It is not a display-name routing alias. Project registrations no longer carry model profiles or selected-provider settings. On migration, the original config bytes are saved once as `config.pre-cadenza.json`; the previously selected governance binding is preserved as the project's base host config, then retired inference fields disappear from active configuration. Existing host files and session messages are not deleted.

Echo never locates or starts Cadenza. `/settings` → **Cadenza connection** configures the global WebSocket URL and credential reference. `env:NAME` reads an existing environment variable; `windows:Amity/Echo/Cadenza/name` reads an existing Windows Credential Manager credential. The service and Echo need the same credential, but Echo does not generate, display, or place it in JSON, prompts, URLs, or command arguments. Python/Rust process memory zeroization is not promised.

Plaintext `ws://` is accepted only for literal loopback IP addresses. Remote endpoints require `wss://` with normal Rustls certificate verification and WebPKI roots; no insecure certificate override is provided. Userinfo, queries, fragments and paths other than `/cadenza` are rejected. The current Cadenza server itself only listens on loopback; a physical remote TLS deployment has not been accepted.

## Native protocol behavior

The implementation follows Cadenza's actual `docs/protocol.md`, `service.rs`, `worker.rs` and reference client: masked RFC 6455 frames; hello `{type:"hello",versions:[1],token:...}`; negotiated version 1; discover/status; generate with exact `model_root`; accepted/token/complete/error events; cancel by request ID. Echo uses the protocol's JSON text-prompt mode. It does not create an alternative JSON API or OpenAI route. CDB1 token-array inputs are a Cadenza capability not needed by Echo's text conversation path; unnegotiated binary output fails explicitly.

Tungstenite 0.30 handles framing, fragments, ping/pong and Rustls transport. Incoming messages/frames are bounded at 1 MiB; prompt size is bounded to Cadenza's 64 KiB; generation requests use 1..256 output tokens. Request IDs are unique per process. Admission, monotonically ordered token indices, matching IDs, token IDs and completion usage are validated. Cadenza enforces actual tokenizer/context capacity. Echo does not guess token counts, insert BOS/EOS or silently truncate context.

Token event text is the complete decoded generated text so far. Both the model API and terminal treat it as a replacement snapshot. This supports revised incomplete UTF-8 suffixes and prevents cumulative text from being appended twice. The headless JSON event is `model_text` with `cumulative:true`; consumers replace their previous output. For credential redaction, unfinished words can be held until a boundary; the final assistant message contains the complete sanitized response.

Generation IO runs on a worker while the terminal polls input every 25 ms. Ctrl+C sends cancellation and waits up to five seconds for completion; a timeout closes the connection, cancelling owned work. Natural completions validate the server's prompt/output usage against observed events. Current Cadenza cancellation completions omit usage, so cancellation/interruption accounting uses admitted prompt tokens and observed token events, without inventing additional server-side usage or prices. Partial generated text and queued user instructions are preserved on failure.

Interactive startup checks the configured server, readiness and selected identity. Picker/discovery IO also runs off the UI thread. A missing service reports `Cadenza unavailable` and leaves the terminal usable; missing or rejected credentials are reported separately. New turns use fresh authenticated connections; completed/partial streams are never automatically replayed. Service restart recovery is therefore for new requests, not silent continuation of an interrupted request.

## Model picker and sessions

`/model` uses Echo's native full-screen terminal UI. Discovery is dynamic and has no Lyra-only code path. It highlights the active session selection (or global default), shows the exact reported identity/root, optional name/family/context/precision, capability states and readiness, and supports arrows, Enter, Escape and Page Up/Down for details. Missing metadata is omitted. Selection is revalidated before both global config and session are updated.

New sessions inherit the global model. Existing sessions preserve their current Cadenza identity; choosing another model records a change at the current message index. Historical pre-Cadenza model names remain readable and are recorded in model history when a user explicitly selects Cadenza for future turns. New assistant messages record the actual selected root. Old messages are not rewritten to claim they came from a new model.

Ordinary foundation output is `ModelTurn::Text`, which is never parsed as a tool, AIP request, task request or permission. This includes strings that happen to look like JSON. Current Lyra does not advertise structured Echo tool use. The structured agent path remains available only when discovery explicitly advertises the implemented `echo.agent.v2` capability; its existing argument validation, actor scope, approvals and Amity policy checks remain authoritative. Cadenza/1 has no image transport, so image delivery fails explicitly and historical references remain readable.

Amity's task host still requires its existing JSON-over-stdio callback. Echo supplies a minimal first-party `__cadenza_turn` adapter using the same native client. For `task-run`, a temporary binding replaces any stored model command/args with this adapter and pins the session root; no obsolete provider command can be invoked. Governance fields/actors/policies are preserved. Only the configured Cadenza environment credential and existing host/MCP credential references are forwarded; vault credentials need no environment forwarding.

## Reproduction and evidence

Portable/isolated tests: `cargo test --locked`. Real tests are explicitly ignored by that command unless a real service and isolated configuration are supplied. The acceptance runner runs both ignored tests separately and exercises a real ConPTY terminal with keyboard input and captured rendered screens:

```powershell
python -m venv target/qa-env
./target/qa-env/Scripts/python.exe -m pip install pywinpty pyte
cargo build --release --locked
./target/qa-env/Scripts/python.exe -B tools/accept_cadenza.py `
  --cadenza-root '../Amity Cadenza' `
  --cadenza-config '../Amity Cadenza/config/local-lyra.json' `
  --echo target/release/echocli.exe --output target/cadenza-acceptance-new
```

The output directory must be new. The runner generates a process-local API credential, starts the existing Cadenza executable with its existing read-only development config, uses an isolated global Echo home and workspaces, and stops its service/terminal processes. It does not alter the user's global settings or credentials, launch training, rewrite model artifacts, or publish a release. Local private Amity fixture credentials are stored only under ignored test output through Echo's existing workspace initialization mechanism; they are not Cadenza credentials or committed data.

## Verified / pending / blocked

Implemented and physically exercised: dynamic discovery and health; actual Lyra streaming; cumulative rendering; exact final assistant text against direct native-client output; native picker selection/cancel/current highlighting; persisted selection across restart and CWD; cancellation; usage; rejected credentials/unavailable roots; disconnected service UI with preserved selection; new-request recovery after service restart; native Amity model callback; config migration backup; historical session readability; Windows vault round trip; governed file read, capability/policy discovery and rejection of a mutation without an active task.

**Separate Amity baseline failures:** the installed host's `workspace-init` sets `workspace_base` inside `project_root`, which its `agent-tool` correctly rejects as overlapping. The isolated governance fixture uses a non-overlapping candidate directory while retaining the exact ACL/authority/actor configuration; the generated host file is unchanged. Fresh-workspace `task-create` still fails `ACL-CTX005: invalid task targets` because there are no resolved semantic targets. This failure is recorded as `task_creation_passed:false`, not converted into a passing task execution claim. Full governed task creation/execution and the approval-then-execute journey remain blocked by that independent host baseline. No authority checks were relaxed and no accepted decisions/tasks were fabricated. Amity source was read only, and no host binary was replaced.

Pending: physical remote TLS deployment/certificate acceptance; actual instruction/tool-trained models and structured tool-use acceptance; image transport if Cadenza adds it; independent security review and extended transport fuzzing. Lyra foundation quality and the current service context cap (128 in this development configuration) are model/service limits, not proof of a broken WebSocket connection.

Final acceptance on 2026-11-12: **16 isolated tests passed**, with the two service-dependent cases then explicitly run and passing their bounded assertions. The full installed-executable/ConPTY run is `target/cadenza-terminal-accepted/result.json`. It includes actual Ctrl+C cancellation after 13 observed output tokens, preserved partial text/interrupted status, and a separate exact comparison of Echo's assistant message against direct native-client output (5 input / 8 output tokens). Amity task creation remains explicitly failed in that same report.

The initial 0.3.5 source build and established `cargo install --path . --force --locked` installation passed. That installed binary matched the release build byte for byte (SHA-256 `e572adf10f1b70fda2ef6201f433476f1e707aa4fdd106ad9d98689173002240`). Existing dead-code warnings were not suppressed. The owner subsequently authorized a public release; version 0.3.6 packages these native Cadenza changes through the tag-driven workflow. Main pushes alone do not publish releases. Version-specific notes are in `docs/releases/v0.3.6.md`; release build, artifact checksums and publication evidence are kept under ignored `target/release-v0.3.6/`.

Library contracts checked against primary documentation: [Tungstenite WebSocket configuration](https://docs.rs/tungstenite/0.30.0/tungstenite/protocol/struct.WebSocketConfig.html), the matching downloaded crate's TLS/client source, and [Microsoft Credential Manager](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credreadw). Cadenza's local protocol/service/reference client remain the authoritative application protocol.
