# Agents

This repository ships a small in-guest helper called **intar-agent**. It runs inside every scenario VM and evaluates the probes defined in your `.hcl` files, sending the results back to the host over a virtio-serial channel.

## Project layout
- `crates/intar-cli`: CLI entrypoint, build script, and agent embedding.
- `crates/intar-vm`: VM orchestration, cloud-init, runner, and host-side wiring.
- `crates/intar-agent`: Guest-side agent that executes probes.
- `crates/intar-probes`: Shared probe spec + parsing/validation logic (host and guest).
- `crates/intar-ui`: TUI (ratatui/crossterm).
- `scenarios/`: Example scenarios and probe definitions.

## Instruction scope
- This root `AGENTS.md` applies to the entire repository.
- If a subdirectory needs different rules, add a nested `AGENTS.md` (or `AGENTS.override.md`) in that folder.
- Keep instructions concise and split large guidance across nested files when needed.

## Common commands
- `intar start <scenario.hcl>` to run a scenario end-to-end.
- `just check` before shipping changes (fmt + clippy + nextest).

## Change checklist
- When adding a new probe type: update `crates/intar-probes` and `crates/intar-agent` together, then adjust docs in this file.
- When editing scenario probes: include an optional `description` to improve the UI briefing/objectives panels.
- If you touch the agent protocol, update both host and guest handling plus this protocol section.
- Do not delete run artifacts while a scenario is still running; cleanup happens after testing, not during the run.

## Commit messages
Strict Conventional Commits format (no footers):
```
<type>(<scope>): <imperative summary>

Why:
- <root cause / problem>
- <why this approach>

Impact:
- <user-visible impact / risk / perf / compat>
- <tests run or "Tests: not run (reason)">

Breaking:
- <detail>   # only when applicable
```

Rules (strict):
- Subject is imperative, <= 72 chars, no trailing period.
- Scope is required and must be a crate or area: `ui`, `vm`, `agent`, `probes`,
  `cli`, `core`, `docs`, `infra`.
- Body is required for every commit and must include **Why** and **Impact**
  sections exactly as shown.
- Use bullet points under **Why** and **Impact** (at least one each).
- Do not insert blank lines between bullet points.
- Use **Breaking** only when relevant.
- Wrap body lines at ~72 chars.
- If multiple areas change, pick the dominant scope and mention the others in
  **Impact**.

Allowed types: `feat`, `fix`, `refactor`, `docs`, `test`, `style`, `chore`.

Examples:
```
feat(ui): add mission briefing screen

Why:
- make objectives discoverable before running

Impact:
- adds new briefing tab and pre-run screen
- Tests: not run (ui change only)
```

```
fix(vm): avoid double-boot probes

Why:
- boot checks were retried on every reconnect

Impact:
- reduces boot time and removes duplicate probe hits
- Tests: just check
```

## Safety
- Never run `git restore` without asking first (it discards local changes).
- Always cleanup VM resources (logs, caches, images, snapshots) after testing - not the scenario run.

## Validation
- Run `just check` before shipping changes (fmt + clippy + nextest).

## Lifecycle
- Build time: `crates/intar-cli/build.rs` cross-compiles `intar-agent` for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` via `cargo zigbuild`, then embeds both binaries with `include_bytes!` in `crates/intar-cli/src/agent.rs`. If the build tools are missing, placeholders are written and `intar start` will refuse to run.
- Start-up: `intar start <scenario.hcl>` base64-embeds the correct agent binary into cloud-init (see `crates/intar-vm/src/cloud_init.rs`) and drops a systemd unit that keeps `intar-agent` running.
- Guest side: the agent opens `/dev/virtio-ports/intar.agent` (fallback `/dev/vport0p1`), reads newline-delimited JSON requests, and replies on the same handle.
- Host side: QEMU exposes the virtio-serial port as a Unix socket at `<run_dir>/<vm>-serial.sock`; `ScenarioRunner::wait_for_agents` pings the agent until it responds before probes are dispatched.

## Protocol (newline-delimited JSON)
**Requests**
- `ping`
- `check_probe` `{ id, spec }`
- `check_all` `{ probes: [(id, spec), ...] }`

**Responses**
- `pong` `{ uptime_secs }`
- `probe_result` `{ id, passed, message }`
- `all_results` `{ results: [ { id, passed, message }, ... ] }`
- `error` `{ message }`

Example round-trip:
```
{"type":"ping"}
{"type":"pong","uptime_secs":12}
```

## Probe catalogue (handled inside the guest)
- `file_content`: `path`, optional `contains`, optional `regex`.
- `file_exists`: `path`, `exists` (bool).
- `service`: `service`, `state` (`running|stopped|enabled|disabled`); uses `systemctl`.
- `port`: `port`, `state` (`listening|closed`), optional `protocol` (`tcp` default); uses tokio sockets (TCP connect / UDP bind).
- `tcp_ping`: `host`, optional `port` (default `1`), optional `timeout_ms` (default `2000`), optional `state` (`reachable|unreachable`, default `reachable`).
- `k8s_nodes_ready`: `expected_ready`, optional `kubeconfig`, optional `context`.
- `k8s_endpoints_nonempty`: `namespace`, `name`, optional `kubeconfig`, optional `context`.
- `command`: `cmd`, `exit_code`, optional `stdout_contains`; executed via `sh -c`.
- `http`: `url`, `status`, optional `body_contains`; uses `reqwest` with a 5s timeout.

## Building / refreshing the agent
Prereqs: `cargo install cargo-zigbuild`, `zig` available in `PATH` (e.g., `brew install zig`), and `qemu-img` for end-to-end runs.

```
cargo zigbuild --release --target x86_64-unknown-linux-musl -p intar-agent
cargo zigbuild --release --target aarch64-unknown-linux-musl -p intar-agent
cargo build --release -p intar-cli  # embeds the freshly built agents
```

Artifacts land in `target/<target>/release/intar-agent`; the CLI copies them into `$OUT_DIR/intar-agent-{arch}` during its build script.

## Debugging tips
- Inside a VM: `systemctl status intar-agent` and `journalctl -u intar-agent` show agent logs (it also prints to stderr).
- From the host: inspect the generated cloud-init for a run at `~/.local/state/intar/runs/<run>/logs/<vm>/user-data.yaml` to verify the agent blob is present.
- Serial socket poking: `socat - UNIX-CONNECT:~/.local/state/intar/runs/<run>/<vm>-serial.sock` and send a `{"type":"ping"}` line to confirm connectivity.
- Probe logic is shared with the host in `crates/intar-probes`; edit there when adding new probe types so both sides stay in sync.

## UI notes
- The Logs view shows the SSH session transcript only (input and output). VM console logs are not streamed there.

## Comment guidelines (please read before editing the agent)
- Avoid filler `//` comments that only restate the code; keep the file readable by letting the code speak for itself.
- Add a short comment only when behavior is non-obvious (e.g., why we retry on virtio connect, why `/dev/vport0p1` is a fallback, or why a probe command tolerates a specific exit code).
- Prefer logging (`tracing`/`eprintln!`) over comments when you want runtime visibility.
- If a workaround is temporary, note the condition for removal in the comment (e.g., `// remove once cloud-localds is packaged on macOS`).
