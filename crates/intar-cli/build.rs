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
    println!("cargo:rerun-if-env-changed=INTAR_AGENT_X86_64_PATH");
    println!("cargo:rerun-if-env-changed=INTAR_AGENT_AARCH64_PATH");

    let targets = [
        ("x86_64-unknown-linux-musl", "intar-agent-x86_64"),
        ("aarch64-unknown-linux-musl", "intar-agent-aarch64"),
    ];

    if try_copy_prebuilt_agents(&out_dir) {
        return;
    }

    let verbose = env::var("INTAR_ZIGBUILD_LOG").is_ok() || env::var("CI").is_ok();

    for (target, output_name) in targets {
        println!("cargo:warning=Building agent for {target}");

        let mut args = vec![
            "zigbuild",
            "--release",
            "--target",
            target,
            "-p",
            "intar-agent",
        ];
        if verbose {
            args.insert(1, "-v");
        }

        let output = Command::new("cargo")
            .current_dir(workspace_root)
            .args(args)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                if verbose {
                    emit_output(target, "stdout", &out.stdout);
                    emit_output(target, "stderr", &out.stderr);
                }
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
            Ok(out) => {
                emit_output(target, "stdout", &out.stdout);
                emit_output(target, "stderr", &out.stderr);
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

fn try_copy_prebuilt_agents(out_dir: &std::path::Path) -> bool {
    let x86 = match env::var("INTAR_AGENT_X86_64_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(_) => return false,
    };
    let arm = match env::var("INTAR_AGENT_AARCH64_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(_) => return false,
    };

    let x86_dst = out_dir.join("intar-agent-x86_64");
    let arm_dst = out_dir.join("intar-agent-aarch64");

    if copy_agent(&x86, &x86_dst) && copy_agent(&arm, &arm_dst) {
        println!("cargo:warning=Using prebuilt intar-agent binaries from env");
        return true;
    }

    println!("cargo:warning=Prebuilt agent copy failed, falling back to zigbuild");
    false
}

fn copy_agent(src: &PathBuf, dst: &PathBuf) -> bool {
    if !src.exists() {
        println!(
            "cargo:warning=Prebuilt agent not found at {}",
            src.display()
        );
        return false;
    }
    match std::fs::copy(src, dst) {
        Ok(_) => true,
        Err(e) => {
            println!(
                "cargo:warning=Failed to copy prebuilt agent {}: {e}",
                src.display()
            );
            false
        }
    }
}

fn emit_output(target: &str, stream: &str, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        println!("cargo:warning=zigbuild[{target}] {stream}: {line}");
    }
}
