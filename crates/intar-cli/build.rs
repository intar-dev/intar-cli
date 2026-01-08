use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set - this should be set by Cargo"));
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR not set - this should be set by Cargo"),
    );
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to find workspace root from manifest directory");

    println!("cargo:rerun-if-changed=../intar-agent/src");
    println!("cargo:rerun-if-changed=../intar-probes/src");

    let targets = [
        ("x86_64-unknown-linux-musl", "intar-agent-x86_64"),
        ("aarch64-unknown-linux-musl", "intar-agent-aarch64"),
    ];

    for (target, output_name) in targets {
        println!("cargo:warning=Building agent for {target}");

        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "zigbuild",
                "--release",
                "--target",
                target,
                "-p",
                "intar-agent",
            ])
            .status();

        match status {
            Ok(s) if s.success() => {
                let src = workspace_root
                    .join("target")
                    .join(target)
                    .join("release")
                    .join("intar-agent");

                let dst = out_dir.join(output_name);

                if src.exists() {
                    if let Err(e) = std::fs::copy(&src, &dst) {
                        println!("cargo:warning=Failed to copy agent binary: {e}");
                        create_placeholder(&dst);
                    } else {
                        println!("cargo:warning=Agent binary copied to {}", dst.display());
                    }
                } else {
                    println!("cargo:warning=Agent binary not found at {}", src.display());
                    create_placeholder(&dst);
                }
            }
            Ok(_) => {
                println!("cargo:warning=cargo-zigbuild failed for {target}, creating placeholder");
                create_placeholder(&out_dir.join(output_name));
            }
            Err(e) => {
                println!(
                    "cargo:warning=cargo-zigbuild not available ({e}), creating placeholder for {target}"
                );
                create_placeholder(&out_dir.join(output_name));
            }
        }
    }
}

fn create_placeholder(path: &PathBuf) {
    if let Err(e) = std::fs::write(path, b"PLACEHOLDER_AGENT_BINARY") {
        println!(
            "cargo:warning=Failed to create placeholder at {}: {e}",
            path.display()
        );
    }
}
