use crate::CoreError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    pub images: HashMap<String, ImageSpec>,
    pub probes: HashMap<String, ProbeDefinition>,
    pub vms: Vec<VmDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSpec {
    pub name: String,
    pub sources: Vec<ImageSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub arch: String,
    pub url: String,
    pub checksum: String,
}

impl ImageSpec {
    #[must_use]
    pub fn source_for_arch(&self, arch: &str) -> Option<&ImageSource> {
        let normalized = match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        self.sources.iter().find(|s| s.arch == normalized)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeDefinition {
    pub name: String,
    #[serde(rename = "type")]
    pub probe_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub phase: ProbePhase,
    #[serde(flatten)]
    pub config: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProbePhase {
    Boot,
    #[default]
    Scenario,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmDefinition {
    pub name: String,
    pub cpu: u32,
    pub memory: u32,
    pub disk: u32,
    pub image: String,
    pub cloud_init: Option<CloudInitConfig>,
    #[serde(default)]
    pub steps: Vec<VmStep>,
    pub probes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmStep {
    pub name: String,
    pub actions: Vec<VmAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VmAction {
    FileDelete {
        path: String,
    },
    FileWrite {
        path: String,
        content: String,
        permissions: Option<String>,
    },
    FileReplace {
        path: String,
        pattern: String,
        replacement: String,
        #[serde(default)]
        regex: bool,
    },
    Systemctl {
        unit: String,
        action: SystemctlAction,
    },
    Command {
        cmd: String,
    },
    K8sApply {
        manifest: String,
        kubeconfig: Option<String>,
    },
    K8sNamespace {
        name: String,
        kubeconfig: Option<String>,
    },
    K8sDeployment {
        name: String,
        namespace: String,
        image: String,
        replicas: u32,
        labels: HashMap<String, String>,
        container_port: u16,
        kubeconfig: Option<String>,
    },
    K8sService {
        name: String,
        namespace: String,
        selector: HashMap<String, String>,
        port: u16,
        target_port: u16,
        kubeconfig: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SystemctlAction {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
    EnableNow,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CloudInitConfig {
    pub packages: Vec<String>,
    /// Optional cloud-init network v2 YAML snippet (including the top-level `network:` key).
    /// When present, it is emitted directly into user-data so cloud-init applies it during
    /// the early network stage.
    pub network_config: Option<String>,
    pub runcmd: Option<String>,
    pub write_files: Vec<WriteFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFile {
    pub path: String,
    pub content: String,
    pub permissions: Option<String>,
}

impl Scenario {
    /// Parse a scenario from an HCL file path.
    ///
    /// # Errors
    /// Returns `CoreError` if the file cannot be read or the contents cannot be parsed.
    pub fn from_file(path: &Path) -> Result<Self, CoreError> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Parse a scenario from an HCL string.
    ///
    /// # Errors
    /// Returns `CoreError` if the HCL is invalid or required fields are missing.
    pub fn parse(content: &str) -> Result<Self, CoreError> {
        let body: hcl::Body =
            hcl::from_str(content).map_err(|e| CoreError::HclParse(e.to_string()))?;

        let mut scenario_name = String::new();
        let mut description = String::new();
        let mut images = HashMap::new();
        let mut probes = HashMap::new();
        let mut vms = Vec::new();

        for block in body.blocks() {
            if block.identifier.as_str() == "scenario" {
                scenario_name = block
                    .labels
                    .first()
                    .map(|l| l.as_str().to_string())
                    .ok_or_else(|| CoreError::InvalidScenario("Missing scenario name".into()))?;

                if let Some(desc) = block
                    .body
                    .attributes()
                    .find(|a| a.key.as_str() == "description")
                {
                    description = extract_string(&desc.expr)?;
                }

                for inner_block in block.body.blocks() {
                    match inner_block.identifier.as_str() {
                        "image" => {
                            let image = parse_image(inner_block)?;
                            images.insert(image.name.clone(), image);
                        }
                        "probe" => {
                            let probe = parse_probe(inner_block)?;
                            probes.insert(probe.name.clone(), probe);
                        }
                        "vm" => {
                            let vm = parse_vm(inner_block)?;
                            vms.push(vm);
                        }
                        _ => {}
                    }
                }
            }
        }

        if scenario_name.is_empty() {
            return Err(CoreError::InvalidScenario("No scenario block found".into()));
        }

        Ok(Scenario {
            name: scenario_name,
            description,
            images,
            probes,
            vms,
        })
    }

    /// Validate that VM and probe references resolve.
    ///
    /// # Errors
    /// Returns `CoreError` if a VM references an unknown image or probe.
    pub fn validate(&self) -> Result<(), CoreError> {
        for vm in &self.vms {
            if !self.images.contains_key(&vm.image) {
                return Err(CoreError::ImageNotFound(vm.image.clone()));
            }
            for probe_name in &vm.probes {
                if !self.probes.contains_key(probe_name) {
                    return Err(CoreError::ProbeNotFound(probe_name.clone()));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn total_probe_count(&self) -> usize {
        self.vms.iter().map(|vm| vm.probes.len()).sum()
    }
}

fn parse_image(block: &hcl::Block) -> Result<ImageSpec, CoreError> {
    let name = block
        .labels
        .first()
        .map(|l| l.as_str().to_string())
        .ok_or_else(|| CoreError::InvalidScenario("Missing image name".into()))?;

    let mut sources = Vec::new();

    for inner_block in block.body.blocks() {
        if inner_block.identifier.as_str() == "source" {
            let source = parse_image_source(inner_block)?;
            sources.push(source);
        }
    }

    if sources.is_empty() {
        return Err(CoreError::InvalidScenario(format!(
            "Image '{name}' has no sources"
        )));
    }

    Ok(ImageSpec { name, sources })
}

fn parse_image_source(block: &hcl::Block) -> Result<ImageSource, CoreError> {
    let mut arch = String::new();
    let mut url = String::new();
    let mut checksum = String::new();

    for attr in block.body.attributes() {
        match attr.key.as_str() {
            "arch" => arch = extract_string(&attr.expr)?,
            "url" => url = extract_string(&attr.expr)?,
            "checksum" => checksum = extract_string(&attr.expr)?,
            _ => {}
        }
    }

    if arch.is_empty() {
        return Err(CoreError::InvalidScenario(
            "Image source missing 'arch'".into(),
        ));
    }
    if url.is_empty() {
        return Err(CoreError::InvalidScenario(
            "Image source missing 'url'".into(),
        ));
    }
    if checksum.is_empty() {
        return Err(CoreError::InvalidScenario(
            "Image source missing 'checksum' (required for verification)".into(),
        ));
    }

    Ok(ImageSource {
        arch,
        url,
        checksum,
    })
}

fn parse_probe(block: &hcl::Block) -> Result<ProbeDefinition, CoreError> {
    let name = block
        .labels
        .first()
        .map(|l| l.as_str().to_string())
        .ok_or_else(|| CoreError::InvalidScenario("Missing probe name".into()))?;

    let mut probe_type = String::new();
    let mut description: Option<String> = None;
    let mut config = HashMap::new();
    let mut phase = ProbePhase::Scenario;

    for attr in block.body.attributes() {
        let key = attr.key.as_str();
        match key {
            "type" => {
                probe_type = extract_string(&attr.expr)?;
            }
            "description" => {
                description = Some(extract_string(&attr.expr)?);
            }
            "phase" => {
                let val = extract_string(&attr.expr)?;
                phase = match val.as_str() {
                    "boot" => ProbePhase::Boot,
                    "scenario" => ProbePhase::Scenario,
                    other => {
                        return Err(CoreError::InvalidScenario(format!(
                            "Probe '{name}' phase must be 'boot' or 'scenario', got '{other}'"
                        )));
                    }
                };
            }
            _ => {
                config.insert(key.to_string(), expr_to_json(&attr.expr)?);
            }
        }
    }

    if probe_type.is_empty() {
        return Err(CoreError::InvalidScenario(format!(
            "Probe '{name}' missing type"
        )));
    }

    Ok(ProbeDefinition {
        name,
        probe_type,
        description,
        phase,
        config,
    })
}

fn parse_vm(block: &hcl::Block) -> Result<VmDefinition, CoreError> {
    let name = block
        .labels
        .first()
        .map(|l| l.as_str().to_string())
        .ok_or_else(|| CoreError::InvalidScenario("Missing VM name".into()))?;

    let mut cpu: u32 = 1;
    let mut memory = 1024;
    let mut disk = 10;
    let mut image = String::new();
    let mut cloud_init = CloudInitConfig::default();
    let mut steps: Vec<VmStep> = Vec::new();
    let mut probes = Vec::new();

    for attr in block.body.attributes() {
        match attr.key.as_str() {
            "cpu" => cpu = extract_u32(&attr.expr)?,
            "memory" => memory = extract_u32(&attr.expr)?,
            "disk" => disk = extract_u32(&attr.expr)?,
            "image" => image = extract_string(&attr.expr)?,
            "probes" => probes = extract_string_array(&attr.expr)?,
            _ => {}
        }
    }

    for inner_block in block.body.blocks() {
        match inner_block.identifier.as_str() {
            "cloud_init" => {
                cloud_init = parse_cloud_init(inner_block)?;
            }
            "step" => {
                steps.push(parse_vm_step(inner_block)?);
            }
            _ => {}
        }
    }

    let mut seen_step_names: HashSet<&str> = HashSet::new();
    for step in &steps {
        if !seen_step_names.insert(step.name.as_str()) {
            return Err(CoreError::InvalidScenario(format!(
                "VM '{name}' has duplicate step '{}'.",
                step.name
            )));
        }
    }

    if image.is_empty() {
        return Err(CoreError::InvalidScenario(format!(
            "VM '{name}' missing image"
        )));
    }

    if cpu == 0 {
        return Err(CoreError::InvalidScenario(format!(
            "VM '{name}' cpu must be > 0"
        )));
    }

    Ok(VmDefinition {
        name,
        cpu,
        memory,
        disk,
        image,
        cloud_init: Some(cloud_init),
        steps,
        probes,
    })
}

fn parse_vm_step(block: &hcl::Block) -> Result<VmStep, CoreError> {
    let name = block
        .labels
        .first()
        .map(|l| l.as_str().to_string())
        .ok_or_else(|| CoreError::InvalidScenario("step block missing name".into()))?;

    let mut actions: Vec<VmAction> = Vec::new();
    for inner_block in block.body.blocks() {
        let action = parse_vm_action(inner_block)?;
        actions.push(action);
    }

    if actions.is_empty() {
        return Err(CoreError::InvalidScenario(format!(
            "step '{name}' must contain at least one action block"
        )));
    }

    Ok(VmStep { name, actions })
}

fn parse_vm_action(block: &hcl::Block) -> Result<VmAction, CoreError> {
    match block.identifier.as_str() {
        "file_delete" => Ok(VmAction::FileDelete {
            path: extract_required_attr_string(block, "path")?,
        }),
        "file_write" => Ok(VmAction::FileWrite {
            path: extract_required_attr_string(block, "path")?,
            content: extract_required_attr_string(block, "content")?,
            permissions: extract_optional_attr_string(block, "permissions")?,
        }),
        "file_replace" => Ok(VmAction::FileReplace {
            path: extract_required_attr_string(block, "path")?,
            pattern: extract_required_attr_string(block, "pattern")?,
            replacement: extract_required_attr_string(block, "replacement")?,
            regex: extract_optional_attr_bool(block, "regex")?.unwrap_or(false),
        }),
        "systemctl" => Ok(VmAction::Systemctl {
            unit: extract_required_attr_string(block, "unit")?,
            action: parse_systemctl_action(&extract_required_attr_string(block, "action")?)?,
        }),
        "command" => Ok(VmAction::Command {
            cmd: extract_required_attr_string(block, "cmd")?,
        }),
        "k8s_apply" => {
            reject_attr(block, "kubectl")?;
            Ok(VmAction::K8sApply {
                manifest: extract_required_attr_string(block, "manifest")?,
                kubeconfig: extract_optional_attr_string(block, "kubeconfig")?,
            })
        }
        "k8s_namespace" => {
            reject_attr(block, "kubectl")?;
            Ok(VmAction::K8sNamespace {
                name: extract_required_attr_string(block, "name")?,
                kubeconfig: extract_optional_attr_string(block, "kubeconfig")?,
            })
        }
        "k8s_deployment" => {
            reject_attr(block, "kubectl")?;
            Ok(VmAction::K8sDeployment {
                name: extract_required_attr_string(block, "name")?,
                namespace: extract_required_attr_string(block, "namespace")?,
                image: extract_required_attr_string(block, "image")?,
                replicas: extract_optional_attr_u32(block, "replicas")?.unwrap_or(1),
                labels: extract_optional_attr_string_map(block, "labels")?.unwrap_or_else(|| {
                    HashMap::from([("app".into(), step_default_app_label(block))])
                }),
                container_port: extract_required_attr_u16(block, "container_port")?,
                kubeconfig: extract_optional_attr_string(block, "kubeconfig")?,
            })
        }
        "k8s_service" => {
            reject_attr(block, "kubectl")?;
            Ok(VmAction::K8sService {
                name: extract_required_attr_string(block, "name")?,
                namespace: extract_required_attr_string(block, "namespace")?,
                selector: extract_required_attr_string_map(block, "selector")?,
                port: extract_required_attr_u16(block, "port")?,
                target_port: extract_optional_attr_u16(block, "target_port")?
                    .unwrap_or(extract_required_attr_u16(block, "port")?),
                kubeconfig: extract_optional_attr_string(block, "kubeconfig")?,
            })
        }
        other => Err(CoreError::InvalidScenario(format!(
            "Unknown action '{other}' in step block"
        ))),
    }
}

fn parse_systemctl_action(action: &str) -> Result<SystemctlAction, CoreError> {
    match action {
        "start" => Ok(SystemctlAction::Start),
        "stop" => Ok(SystemctlAction::Stop),
        "restart" => Ok(SystemctlAction::Restart),
        "enable" => Ok(SystemctlAction::Enable),
        "disable" => Ok(SystemctlAction::Disable),
        "enable_now" => Ok(SystemctlAction::EnableNow),
        other => Err(CoreError::InvalidScenario(format!(
            "Unknown systemctl action '{other}' (expected start|stop|restart|enable|disable|enable_now)"
        ))),
    }
}

fn parse_cloud_init(block: &hcl::Block) -> Result<CloudInitConfig, CoreError> {
    let mut config = CloudInitConfig::default();

    for attr in block.body.attributes() {
        match attr.key.as_str() {
            "packages" => config.packages = extract_string_array(&attr.expr)?,
            "network_config" => config.network_config = Some(extract_string(&attr.expr)?),
            "runcmd" => config.runcmd = Some(extract_string(&attr.expr)?),
            _ => {}
        }
    }

    for inner_block in block.body.blocks() {
        if inner_block.identifier.as_str() == "write_file" {
            config.write_files.push(parse_write_file(inner_block)?);
        }
    }

    Ok(config)
}

fn parse_write_file(block: &hcl::Block) -> Result<WriteFile, CoreError> {
    let mut path = String::new();
    let mut content = String::new();
    let mut permissions = None;

    for attr in block.body.attributes() {
        match attr.key.as_str() {
            "path" => path = extract_string(&attr.expr)?,
            "content" => content = extract_string(&attr.expr)?,
            "permissions" => permissions = Some(extract_string(&attr.expr)?),
            _ => {}
        }
    }

    if path.is_empty() {
        return Err(CoreError::InvalidScenario(
            "write_file block missing 'path'".into(),
        ));
    }

    if content.is_empty() {
        return Err(CoreError::InvalidScenario(
            "write_file block missing 'content'".into(),
        ));
    }

    Ok(WriteFile {
        path,
        content,
        permissions,
    })
}

fn extract_string(expr: &hcl::Expression) -> Result<String, CoreError> {
    match expr {
        hcl::Expression::String(s) => Ok(s.clone()),
        hcl::Expression::TemplateExpr(t) => Ok(t.to_string().trim_matches('"').to_string()),
        _ => Err(CoreError::InvalidScenario(format!(
            "Expected string, got {expr:?}"
        ))),
    }
}

fn extract_bool(expr: &hcl::Expression) -> Result<bool, CoreError> {
    match expr {
        hcl::Expression::Bool(b) => Ok(*b),
        _ => Err(CoreError::InvalidScenario(format!(
            "Expected bool, got {expr:?}"
        ))),
    }
}

fn extract_u32(expr: &hcl::Expression) -> Result<u32, CoreError> {
    match expr {
        hcl::Expression::Number(n) => n
            .as_u64()
            .ok_or_else(|| CoreError::InvalidScenario("Invalid number".into()))
            .and_then(|v| {
                u32::try_from(v).map_err(|_| CoreError::InvalidScenario("Invalid number".into()))
            }),
        _ => Err(CoreError::InvalidScenario(format!(
            "Expected number, got {expr:?}"
        ))),
    }
}

fn extract_u16(expr: &hcl::Expression) -> Result<u16, CoreError> {
    let value = extract_u32(expr)?;
    u16::try_from(value).map_err(|_| CoreError::InvalidScenario("Invalid number".into()))
}

fn extract_optional_attr_string(
    block: &hcl::Block,
    key: &str,
) -> Result<Option<String>, CoreError> {
    block
        .body
        .attributes()
        .find(|a| a.key.as_str() == key)
        .map(|attr| extract_string(&attr.expr))
        .transpose()
}

fn reject_attr(block: &hcl::Block, key: &str) -> Result<(), CoreError> {
    if block.body.attributes().any(|a| a.key.as_str() == key) {
        return Err(CoreError::InvalidScenario(format!(
            "{} block does not support attribute '{key}'",
            block.identifier
        )));
    }
    Ok(())
}

fn extract_required_attr_string(block: &hcl::Block, key: &str) -> Result<String, CoreError> {
    extract_optional_attr_string(block, key)?.ok_or_else(|| {
        CoreError::InvalidScenario(format!(
            "{} block missing required attribute '{key}'",
            block.identifier
        ))
    })
}

fn extract_optional_attr_u32(block: &hcl::Block, key: &str) -> Result<Option<u32>, CoreError> {
    block
        .body
        .attributes()
        .find(|a| a.key.as_str() == key)
        .map(|attr| extract_u32(&attr.expr))
        .transpose()
}

fn extract_optional_attr_u16(block: &hcl::Block, key: &str) -> Result<Option<u16>, CoreError> {
    block
        .body
        .attributes()
        .find(|a| a.key.as_str() == key)
        .map(|attr| extract_u16(&attr.expr))
        .transpose()
}

fn extract_required_attr_u16(block: &hcl::Block, key: &str) -> Result<u16, CoreError> {
    extract_optional_attr_u16(block, key)?.ok_or_else(|| {
        CoreError::InvalidScenario(format!(
            "{} block missing required attribute '{key}'",
            block.identifier
        ))
    })
}

fn extract_optional_attr_bool(block: &hcl::Block, key: &str) -> Result<Option<bool>, CoreError> {
    block
        .body
        .attributes()
        .find(|a| a.key.as_str() == key)
        .map(|attr| extract_bool(&attr.expr))
        .transpose()
}

fn extract_string_map(expr: &hcl::Expression) -> Result<HashMap<String, String>, CoreError> {
    match expr {
        hcl::Expression::Object(obj) => obj
            .iter()
            .map(|(k, v)| Ok((k.to_string(), extract_string(v)?)))
            .collect(),
        _ => Err(CoreError::InvalidScenario(format!(
            "Expected object, got {expr:?}"
        ))),
    }
}

fn extract_optional_attr_string_map(
    block: &hcl::Block,
    key: &str,
) -> Result<Option<HashMap<String, String>>, CoreError> {
    block
        .body
        .attributes()
        .find(|a| a.key.as_str() == key)
        .map(|attr| extract_string_map(&attr.expr))
        .transpose()
}

fn extract_required_attr_string_map(
    block: &hcl::Block,
    key: &str,
) -> Result<HashMap<String, String>, CoreError> {
    extract_optional_attr_string_map(block, key)?.ok_or_else(|| {
        CoreError::InvalidScenario(format!(
            "{} block missing required attribute '{key}'",
            block.identifier
        ))
    })
}

fn step_default_app_label(block: &hcl::Block) -> String {
    block
        .body
        .attributes()
        .find(|a| a.key.as_str() == "name")
        .map_or_else(
            || "app".into(),
            |attr| extract_string(&attr.expr).unwrap_or_else(|_| "app".into()),
        )
}

fn extract_string_array(expr: &hcl::Expression) -> Result<Vec<String>, CoreError> {
    match expr {
        hcl::Expression::Array(arr) => arr.iter().map(extract_string).collect(),
        _ => Err(CoreError::InvalidScenario(format!(
            "Expected array, got {expr:?}"
        ))),
    }
}

fn expr_to_json(expr: &hcl::Expression) -> Result<serde_json::Value, CoreError> {
    match expr {
        hcl::Expression::String(s) => Ok(serde_json::Value::String(s.clone())),
        hcl::Expression::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(serde_json::Value::Number(i.into()))
            } else if let Some(f) = n.as_f64() {
                Ok(serde_json::json!(f))
            } else {
                Ok(serde_json::Value::Null)
            }
        }
        hcl::Expression::Bool(b) => Ok(serde_json::Value::Bool(*b)),
        hcl::Expression::Array(arr) => {
            let values: Result<Vec<_>, _> = arr.iter().map(expr_to_json).collect();
            Ok(serde_json::Value::Array(values?))
        }
        _ => Ok(serde_json::Value::String(format!("{expr:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scenario() {
        let hcl = r#"
scenario "test-scenario" {
  description = "A test scenario"

  image "ubuntu-24.04" {
    source {
      arch     = "amd64"
      url      = "https://example.com/ubuntu-amd64.qcow2"
      checksum = "sha256:abc123"
    }
    source {
      arch     = "arm64"
      url      = "https://example.com/ubuntu-arm64.qcow2"
      checksum = "sha256:def456"
    }
  }

  probe "test-probe" {
    type    = "service"
    service = "nginx"
    state   = "running"
    description = "Ensure nginx is running"
  }

  vm "webserver" {
    cpu    = 2
    memory = 2048
    disk   = 10
    image  = "ubuntu-24.04"

    cloud_init {
      packages = ["nginx"]
      runcmd = "systemctl stop nginx"
    }

    probes = ["test-probe"]
  }
}
"#;

        let scenario = Scenario::parse(hcl).unwrap();
        assert_eq!(scenario.name, "test-scenario");
        assert_eq!(scenario.description, "A test scenario");
        assert_eq!(scenario.images.len(), 1);
        assert!(scenario.images.contains_key("ubuntu-24.04"));
        assert_eq!(scenario.images["ubuntu-24.04"].sources.len(), 2);
        assert_eq!(scenario.probes.len(), 1);
        assert_eq!(
            scenario.probes["test-probe"].description.as_deref(),
            Some("Ensure nginx is running")
        );
        assert_eq!(scenario.vms.len(), 1);
        assert_eq!(scenario.vms[0].name, "webserver");
        assert_eq!(scenario.vms[0].cpu, 2);
        assert_eq!(scenario.vms[0].image, "ubuntu-24.04");
        assert_eq!(scenario.total_probe_count(), 1);

        scenario.validate().unwrap();
    }

    #[test]
    fn test_parse_write_file() {
        let hcl = r#"
scenario "write-file" {
  description = "Ensure write_file blocks are parsed"

  image "ubuntu-24.04" {
    source {
      arch     = "amd64"
      url      = "https://example.com/ubuntu-amd64.qcow2"
      checksum = "sha256:abc123"
    }
  }

  probe "noop" {
    type    = "service"
    service = "nginx"
    state   = "running"
  }

  vm "web" {
    image = "ubuntu-24.04"
    cloud_init {
      write_file {
        path        = "/tmp/test.txt"
        permissions = "0644"
        content     = "hello world"
      }
    }
    probes = ["noop"]
  }
}
"#;

        let scenario = Scenario::parse(hcl).unwrap();
        let vm = &scenario.vms[0];
        let cloud_init = vm.cloud_init.as_ref().unwrap();
        assert_eq!(cloud_init.write_files.len(), 1);
        let file = &cloud_init.write_files[0];
        assert_eq!(file.path, "/tmp/test.txt");
        assert_eq!(file.content, "hello world");
        assert_eq!(file.permissions.as_deref(), Some("0644"));
    }

    #[test]
    fn test_parse_vm_step_actions() {
        let hcl = r#"
scenario "step-actions" {
  description = "Parse step blocks inside vm"

  image "ubuntu-24.04" {
    source {
      arch     = "amd64"
      url      = "https://example.com/ubuntu-amd64.qcow2"
      checksum = "sha256:abc123"
    }
  }

  probe "noop" {
    type    = "service"
    service = "nginx"
    state   = "running"
  }

  vm "web" {
    image = "ubuntu-24.04"

    step "break-nginx" {
      systemctl {
        unit   = "nginx"
        action = "stop"
      }

      file_delete {
        path = "/etc/nginx/sites-enabled/default"
      }

      file_replace {
        path        = "/etc/nginx/nginx.conf"
        pattern     = "worker_processes auto;"
        replacement = "worker_processes 2;"
      }

      k8s_namespace {
        name = "test"
      }

      k8s_deployment {
        name           = "echo"
        namespace      = "test"
        image          = "nginx:1.27-alpine"
        container_port = 80
        labels         = { app = "echo" }
      }

      k8s_service {
        name        = "echo-svc"
        namespace   = "test"
        selector    = { app = "echo-typo" }
        port        = 80
        target_port = 80
      }

      k8s_apply {
        manifest = "{\"apiVersion\":\"v1\",\"kind\":\"Namespace\",\"metadata\":{\"name\":\"test\"}}"
      }

      command {
        cmd = "echo hello"
      }
    }

    probes = ["noop"]
  }
}
"#;

        let scenario = Scenario::parse(hcl).unwrap();
        let vm = &scenario.vms[0];
        assert_eq!(vm.steps.len(), 1);
        assert_eq!(vm.steps[0].name, "break-nginx");
        assert_eq!(vm.steps[0].actions.len(), 8);
    }

    #[test]
    fn test_k8s_kubectl_override_rejected() {
        let hcl = r#"
scenario "reject-kubectl-override" {
  image "ubuntu-24.04" {
    source {
      arch     = "amd64"
      url      = "https://example.com/ubuntu-amd64.qcow2"
      checksum = "sha256:abc123"
    }
  }

  vm "web" {
    image = "ubuntu-24.04"

    step "apply" {
      k8s_namespace {
        name    = "test"
        kubectl = "k3s kubectl"
      }
    }
  }
}
"#;

        let err = Scenario::parse(hcl).unwrap_err();
        match err {
            CoreError::InvalidScenario(msg) => assert!(msg.contains("kubectl")),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
