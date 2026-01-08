# Task recipes for intar workspace

set shell := ["/bin/bash", "-c"]

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic
	cargo nextest run --workspace

run:
	cargo run --bin intar -- start scenarios/broken-nginx.hcl
