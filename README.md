# intar

QEMU-based DevOps lab environment that runs HCL scenarios in local VMs,
evaluates probes, and presents progress in a TUI.

## Quick start
Requirements:
- Rust toolchain
- QEMU (`qemu-system-*`, `qemu-img`) in PATH

```sh
cargo run --bin intar -- start scenarios/broken-nginx.hcl
```

## Usage
```sh
intar start <scenario.hcl>
intar list --dir <path>
intar ssh <vm-name> [--run <run>] [--command <cmd>]
intar logs [--run <run>] [--vm <vm>] [--log-type console|ssh|system]
```

## Scenario format (HCL)
```hcl
scenario "broken-nginx" {
  description = "Fix a misconfigured nginx server"
  image "ubuntu-24.04" { ... }
  probe "nginx-running" { type = "service" ... }
  vm "webserver" { ... probes = ["nginx-running"] }
}
```

See `scenarios/` for full examples.

## Project layout
- `crates/intar-cli` - CLI entrypoint + agent embedding
- `crates/intar-vm` - VM orchestration + cloud-init
- `crates/intar-agent` - guest-side probe runner
- `crates/intar-probes` - probe specs + validation
- `crates/intar-ui` - TUI

## Development
- Run checks: `just check`
- Rebuild embedded agent (after agent/probe changes):
  - `cargo zigbuild --release --target x86_64-unknown-linux-musl -p intar-agent`
  - `cargo zigbuild --release --target aarch64-unknown-linux-musl -p intar-agent`
  - `cargo build --release -p intar-cli`

## Contributing
Discussions and PRs welcome. Include the scenario file and `intar logs` output
when reporting a problem.

## License
MIT
