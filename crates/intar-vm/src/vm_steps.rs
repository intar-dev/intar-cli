use crate::VmError;
use intar_core::{CloudInitConfig, SystemctlAction, VmAction, VmStep, WriteFile};
use std::collections::HashMap;
use std::fmt::Write as _;

/// Compile VM `step` actions into cloud-init `write_files` + `runcmd` entries.
///
/// # Errors
/// Returns `VmError::CloudInit` if the generated scripts cannot be rendered.
pub fn apply_vm_steps_to_cloud_init(
    vm_name: &str,
    steps: &[VmStep],
    config: &mut CloudInitConfig,
) -> Result<(), VmError> {
    if steps.is_empty() {
        return Ok(());
    }

    let mut runcmd = config.runcmd.clone().unwrap_or_default();
    let vm_slug = slugify(vm_name);

    for step in steps {
        let step_slug = slugify(&step.name);
        let hidden = is_hidden_step(step);
        let script_path = if hidden {
            format!("/run/intar-step-{vm_slug}-{step_slug}.sh")
        } else {
            format!("/usr/local/bin/intar-step-{vm_slug}-{step_slug}.sh")
        };
        let script = render_step_script(&vm_slug, &step_slug, step, hidden)?;

        config.write_files.push(WriteFile {
            path: script_path.clone(),
            content: script,
            permissions: Some("0755".into()),
        });

        if hidden {
            append_runcmd_line(&mut runcmd, &format!("bash {script_path}"));
        } else {
            append_runcmd_line(
                &mut runcmd,
                &format!("cloud-init-per once intar-step-{vm_slug}-{step_slug} {script_path}"),
            );
        }
    }

    config.runcmd = Some(runcmd);
    Ok(())
}

fn render_step_script(
    vm_slug: &str,
    step_slug: &str,
    step: &VmStep,
    hidden: bool,
) -> Result<String, VmError> {
    let mut script = String::new();

    render_step_header(&mut script, vm_slug, step_slug, hidden)?;

    for (idx, action) in step.actions.iter().enumerate() {
        render_action(&mut script, step_slug, idx, action)?;
    }

    render_step_footer(&mut script, vm_slug, step_slug, hidden)?;

    Ok(script)
}

fn render_step_header(
    script: &mut String,
    vm_slug: &str,
    step_slug: &str,
    hidden: bool,
) -> Result<(), VmError> {
    writeln!(script, "#!/usr/bin/env bash")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "set -euo pipefail").map_err(|_| VmError::CloudInit("format error".into()))?;

    if hidden {
        writeln!(script, "trap 'rm -f -- \"$0\"' EXIT")
            .map_err(|_| VmError::CloudInit("format error".into()))?;
        writeln!(script, "exec >/dev/null 2>&1")
            .map_err(|_| VmError::CloudInit("format error".into()))?;
    } else {
        writeln!(script, "LOG_DIR=/var/log/intar")
            .map_err(|_| VmError::CloudInit("format error".into()))?;
        writeln!(script, "mkdir -p \"$LOG_DIR\"")
            .map_err(|_| VmError::CloudInit("format error".into()))?;
        writeln!(
            script,
            "exec >\"$LOG_DIR/step-{vm_slug}-{step_slug}.log\" 2>&1"
        )
        .map_err(|_| VmError::CloudInit("format error".into()))?;
        writeln!(
            script,
            "echo \"[intar] step {vm_slug}/{step_slug} starting\""
        )
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    }

    Ok(())
}

fn render_step_footer(
    script: &mut String,
    vm_slug: &str,
    step_slug: &str,
    hidden: bool,
) -> Result<(), VmError> {
    if !hidden {
        writeln!(
            script,
            "echo \"[intar] step {vm_slug}/{step_slug} complete\""
        )
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    }
    Ok(())
}

fn is_hidden_step(step: &VmStep) -> bool {
    let name = step.name.to_lowercase();
    name.starts_with("break") || name.contains("break-") || name.contains("break_")
}

fn render_action(
    script: &mut String,
    step_slug: &str,
    idx: usize,
    action: &VmAction,
) -> Result<(), VmError> {
    match action {
        VmAction::FileDelete { path } => render_file_delete(script, path),
        VmAction::FileWrite {
            path,
            content,
            permissions,
        } => render_file_write(
            script,
            step_slug,
            idx,
            path,
            content,
            permissions.as_deref(),
        ),
        VmAction::FileReplace {
            path,
            pattern,
            replacement,
            regex,
        } => render_file_replace(script, path, pattern, replacement, *regex),
        VmAction::Systemctl { unit, action } => render_systemctl(script, unit, *action),
        VmAction::Command { cmd } => {
            render_command(script, cmd);
            Ok(())
        }
        VmAction::K8sApply {
            manifest,
            kubeconfig,
        } => render_k8s_apply(
            script,
            &k8s_ctx(step_slug, idx, kubeconfig.as_deref()),
            manifest,
        ),
        VmAction::K8sNamespace { name, kubeconfig } => render_k8s_namespace(
            script,
            &k8s_ctx(step_slug, idx, kubeconfig.as_deref()),
            name,
        ),
        VmAction::K8sDeployment {
            name,
            namespace,
            image,
            replicas,
            labels,
            container_port,
            kubeconfig,
        } => render_k8s_deployment(
            script,
            &k8s_ctx(step_slug, idx, kubeconfig.as_deref()),
            K8sDeploymentSpec {
                name,
                namespace,
                image,
                replicas: *replicas,
                labels,
                container_port: *container_port,
            },
        ),
        VmAction::K8sService {
            name,
            namespace,
            selector,
            port,
            target_port,
            kubeconfig,
        } => render_k8s_service(
            script,
            &k8s_ctx(step_slug, idx, kubeconfig.as_deref()),
            K8sServiceSpec {
                name,
                namespace,
                selector,
                port: *port,
                target_port: *target_port,
            },
        ),
    }
}

fn render_file_delete(script: &mut String, path: &str) -> Result<(), VmError> {
    writeln!(script, "rm -f -- {}", shell_quote(path))
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    Ok(())
}

fn render_file_write(
    script: &mut String,
    step_slug: &str,
    idx: usize,
    path: &str,
    content: &str,
    permissions: Option<&str>,
) -> Result<(), VmError> {
    let marker = format!("INTAR_EOF_{step_slug}_{idx}");
    writeln!(
        script,
        "install -d -m 0755 -- \"$(dirname -- {})\"",
        shell_quote(path)
    )
    .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "cat <<'{marker}' > {}", shell_quote(path))
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    script.push_str(content);
    if !content.ends_with('\n') {
        script.push('\n');
    }
    writeln!(script, "{marker}").map_err(|_| VmError::CloudInit("format error".into()))?;
    if let Some(perm) = permissions {
        writeln!(script, "chmod {} -- {}", perm, shell_quote(path))
            .map_err(|_| VmError::CloudInit("format error".into()))?;
    }
    Ok(())
}

fn render_file_replace(
    script: &mut String,
    path: &str,
    pattern: &str,
    replacement: &str,
    regex: bool,
) -> Result<(), VmError> {
    let path_lit = serde_json::to_string(path)
        .map_err(|e| VmError::CloudInit(format!("Failed to encode path: {e}")))?;
    let pattern_lit = serde_json::to_string(pattern)
        .map_err(|e| VmError::CloudInit(format!("Failed to encode pattern: {e}")))?;
    let replacement_lit = serde_json::to_string(replacement)
        .map_err(|e| VmError::CloudInit(format!("Failed to encode replacement: {e}")))?;

    writeln!(script, "python3 - <<'PY'").map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "from pathlib import Path")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "import re").map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "path = {path_lit}").map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "pattern = {pattern_lit}")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "replacement = {replacement_lit}")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "data = Path(path).read_text(encoding='utf-8')")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    if regex {
        writeln!(
            script,
            "new = re.sub(pattern, replacement, data, flags=re.MULTILINE)"
        )
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    } else {
        writeln!(script, "new = data.replace(pattern, replacement)")
            .map_err(|_| VmError::CloudInit("format error".into()))?;
    }
    writeln!(script, "Path(path).write_text(new, encoding='utf-8')")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "PY").map_err(|_| VmError::CloudInit("format error".into()))?;
    Ok(())
}

fn render_systemctl(
    script: &mut String,
    unit: &str,
    action: SystemctlAction,
) -> Result<(), VmError> {
    let systemctl_action = match action {
        SystemctlAction::Start => "start",
        SystemctlAction::Stop => "stop",
        SystemctlAction::Restart => "restart",
        SystemctlAction::Enable => "enable",
        SystemctlAction::Disable => "disable",
        SystemctlAction::EnableNow => "enable --now",
    };
    writeln!(script, "systemctl {systemctl_action} {}", shell_quote(unit))
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    Ok(())
}

fn render_command(script: &mut String, cmd: &str) {
    script.push('\n');
    script.push_str(cmd);
    if !cmd.ends_with('\n') {
        script.push('\n');
    }
}

fn k8s_ctx<'a>(step_slug: &'a str, idx: usize, kubeconfig: Option<&'a str>) -> K8sRenderCtx<'a> {
    K8sRenderCtx {
        step_slug,
        idx,
        kubeconfig,
    }
}

struct K8sRenderCtx<'a> {
    step_slug: &'a str,
    idx: usize,
    kubeconfig: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct K8sDeploymentSpec<'a> {
    name: &'a str,
    namespace: &'a str,
    image: &'a str,
    replicas: u32,
    labels: &'a HashMap<String, String>,
    container_port: u16,
}

#[derive(Clone, Copy)]
struct K8sServiceSpec<'a> {
    name: &'a str,
    namespace: &'a str,
    selector: &'a HashMap<String, String>,
    port: u16,
    target_port: u16,
}

fn render_k8s_apply(
    script: &mut String,
    ctx: &K8sRenderCtx<'_>,
    manifest: &str,
) -> Result<(), VmError> {
    let marker = format!("INTAR_K8S_MANIFEST_{}_{}", ctx.step_slug, ctx.idx);
    render_kubeconfig_selection(script, ctx.kubeconfig)?;
    writeln!(script, "cat <<'{marker}' | kubectl apply -f -")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    script.push_str(manifest);
    if !manifest.ends_with('\n') {
        script.push('\n');
    }
    writeln!(script, "{marker}").map_err(|_| VmError::CloudInit("format error".into()))?;
    Ok(())
}

fn render_kubeconfig_selection(
    script: &mut String,
    kubeconfig: Option<&str>,
) -> Result<(), VmError> {
    if let Some(cfg) = kubeconfig {
        writeln!(script, "export KUBECONFIG={}", shell_quote(cfg))
            .map_err(|_| VmError::CloudInit("format error".into()))?;
        return Ok(());
    }

    writeln!(script, "if [ -z \"${{KUBECONFIG:-}}\" ]; then")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "  if [ -f /etc/rancher/k3s/k3s.yaml ]; then")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "    export KUBECONFIG=/etc/rancher/k3s/k3s.yaml")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "  elif [ -f /etc/kubernetes/admin.conf ]; then")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "    export KUBECONFIG=/etc/kubernetes/admin.conf")
        .map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "  fi").map_err(|_| VmError::CloudInit("format error".into()))?;
    writeln!(script, "fi").map_err(|_| VmError::CloudInit("format error".into()))?;
    Ok(())
}

fn render_k8s_namespace(
    script: &mut String,
    ctx: &K8sRenderCtx<'_>,
    name: &str,
) -> Result<(), VmError> {
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": { "name": name },
    }))
    .map_err(|e| VmError::CloudInit(format!("Failed to encode k8s namespace manifest: {e}")))?;
    render_k8s_apply(script, ctx, &manifest)
}

fn render_k8s_deployment(
    script: &mut String,
    ctx: &K8sRenderCtx<'_>,
    deployment: K8sDeploymentSpec<'_>,
) -> Result<(), VmError> {
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": deployment.name, "namespace": deployment.namespace },
        "spec": {
            "replicas": deployment.replicas,
            "selector": { "matchLabels": deployment.labels },
            "template": {
                "metadata": { "labels": deployment.labels },
                "spec": {
                    "containers": [{
                        "name": deployment.name,
                        "image": deployment.image,
                        "ports": [{ "containerPort": deployment.container_port }],
                    }],
                },
            },
        },
    }))
    .map_err(|e| VmError::CloudInit(format!("Failed to encode k8s deployment manifest: {e}")))?;
    render_k8s_apply(script, ctx, &manifest)
}

fn render_k8s_service(
    script: &mut String,
    ctx: &K8sRenderCtx<'_>,
    service: K8sServiceSpec<'_>,
) -> Result<(), VmError> {
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": { "name": service.name, "namespace": service.namespace },
        "spec": {
            "selector": service.selector,
            "ports": [{ "port": service.port, "targetPort": service.target_port }],
        },
    }))
    .map_err(|e| VmError::CloudInit(format!("Failed to encode k8s service manifest: {e}")))?;
    render_k8s_apply(script, ctx, &manifest)
}

fn append_runcmd_line(runcmd: &mut String, line: &str) {
    if !runcmd.is_empty() && !runcmd.ends_with('\n') {
        runcmd.push('\n');
    }
    runcmd.push_str(line);
    runcmd.push('\n');
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch == '-' || ch == '_' {
            Some(ch)
        } else {
            None
        };

        match normalized {
            Some(ch) => {
                out.push(ch);
                last_dash = false;
            }
            None => {
                if !out.is_empty() && !last_dash {
                    out.push('-');
                    last_dash = true;
                }
            }
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() { "step".into() } else { out }
}

fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_vm_steps_to_cloud_init() {
        let steps = vec![VmStep {
            name: "break-nginx".into(),
            actions: vec![
                VmAction::Systemctl {
                    unit: "nginx".into(),
                    action: SystemctlAction::Stop,
                },
                VmAction::FileDelete {
                    path: "/etc/nginx/sites-enabled/default".into(),
                },
                VmAction::K8sNamespace {
                    name: "test".into(),
                    kubeconfig: Some("/etc/rancher/k3s/k3s.yaml".into()),
                },
            ],
        }];

        let mut config = CloudInitConfig {
            packages: vec!["nginx".into()],
            network_config: None,
            runcmd: Some("echo pre\n".into()),
            write_files: Vec::new(),
        };

        apply_vm_steps_to_cloud_init("web", &steps, &mut config).unwrap();

        let runcmd = config.runcmd.as_deref().unwrap();
        assert!(runcmd.contains("echo pre"));
        assert!(runcmd.contains("bash /run/intar-step-web-break-nginx.sh"));

        let script = config
            .write_files
            .iter()
            .find(|f| f.path == "/run/intar-step-web-break-nginx.sh")
            .map(|f| f.content.as_str())
            .unwrap();
        assert!(script.contains("trap 'rm -f -- \"$0\"' EXIT"));
        assert!(script.contains("exec >/dev/null 2>&1"));
        assert!(script.contains("systemctl stop 'nginx'"));
        assert!(script.contains("rm -f -- '/etc/nginx/sites-enabled/default'"));
        assert!(script.contains("export KUBECONFIG='/etc/rancher/k3s/k3s.yaml'"));
        assert!(script.contains("| kubectl apply -f -"));
    }
}
