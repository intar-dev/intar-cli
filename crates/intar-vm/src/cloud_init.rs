use crate::{VmError, path_to_str};
use intar_core::CloudInitConfig;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

pub struct CloudInitGenerator {
    pub ssh_public_key: String,
    pub agent_binary: Vec<u8>,
}

const DEFAULT_MASK_UNITS: &[&str] = &[
    "apt-daily.service",
    "apt-daily.timer",
    "apt-daily-upgrade.service",
    "apt-daily-upgrade.timer",
    "motd-news.service",
    "motd-news.timer",
    "unattended-upgrades.service",
    "man-db.service",
    "man-db.timer",
    "fstrim.service",
    "fstrim.timer",
    "e2scrub_all.service",
    "e2scrub_all.timer",
    "ua-timer.service",
    "ua-timer.timer",
    "snapd.service",
    "snapd.socket",
    "snapd.seeded.service",
    "snapd.autoimport.service",
];

impl CloudInitGenerator {
    #[must_use]
    pub fn new(ssh_public_key: String, agent_binary: Vec<u8>) -> Self {
        Self {
            ssh_public_key,
            agent_binary,
        }
    }

    #[must_use]
    pub fn generate_user_data(&self, config: &CloudInitConfig, hostname: &str) -> String {
        let agent_base64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &self.agent_binary,
        );

        let mut user_data = String::from("#cloud-config\n");

        let _ = writeln!(user_data, "hostname: {hostname}");
        user_data.push_str("package_update: false\n");
        user_data.push_str("package_upgrade: false\n");

        user_data.push_str("users:\n");
        user_data.push_str("  - name: user\n");
        user_data.push_str("    sudo: ALL=(ALL) NOPASSWD:ALL\n");
        user_data.push_str("    shell: /usr/local/bin/intar-shell\n");
        user_data.push_str("    ssh_authorized_keys:\n");
        let _ = writeln!(user_data, "      - {}", self.ssh_public_key);

        if !config.packages.is_empty() {
            user_data.push_str("packages:\n");
            for pkg in &config.packages {
                let _ = writeln!(user_data, "  - {pkg}");
            }
        }

        user_data.push_str("write_files:\n");
        user_data.push_str("  - path: /usr/local/bin/intar-agent\n");
        user_data.push_str("    permissions: '0755'\n");
        user_data.push_str("    encoding: base64\n");
        let _ = writeln!(user_data, "    content: {agent_base64}");

        user_data.push_str("  - path: /usr/local/bin/intar-shell\n");
        user_data.push_str("    permissions: '0755'\n");
        user_data.push_str("    content: |\n");
        user_data.push_str("      #!/usr/bin/env bash\n");
        user_data.push_str("      set -euo pipefail\n");
        user_data.push_str("      REAL_SHELL=/bin/bash\n");
        user_data.push_str("      AGENT=/usr/local/bin/intar-agent\n");
        user_data.push_str("      \n");
        user_data.push_str("      if [ \"${1:-}\" = \"-c\" ]; then\n");
        user_data.push_str("        cmd=\"${2:-}\"\n");
        user_data.push_str("        exec \"$AGENT\" record-command \"$REAL_SHELL\" \"$cmd\"\n");
        user_data.push_str("      fi\n");
        user_data.push_str("      \n");
        user_data.push_str("      exec \"$AGENT\" record-ssh \"$REAL_SHELL\"\n");

        user_data.push_str("  - path: /etc/systemd/system/intar-agent.service\n");
        user_data.push_str("    content: |\n");
        user_data.push_str("      [Unit]\n");
        user_data.push_str("      Description=Intar Probe Agent\n");
        user_data.push_str("      After=multi-user.target\n");
        user_data.push_str("      \n");
        user_data.push_str("      [Service]\n");
        user_data.push_str("      Type=simple\n");
        user_data.push_str("      ExecStart=/usr/local/bin/intar-agent\n");
        user_data.push_str("      RuntimeDirectory=intar\n");
        user_data.push_str("      RuntimeDirectoryMode=0755\n");
        user_data.push_str("      Restart=always\n");
        user_data.push_str("      RestartSec=1\n");
        user_data.push_str("      \n");
        user_data.push_str("      [Install]\n");
        user_data.push_str("      WantedBy=multi-user.target\n");

        for file in &config.write_files {
            let _ = writeln!(user_data, "  - path: {}", file.path);
            if let Some(permissions) = &file.permissions {
                let _ = writeln!(user_data, "    permissions: '{permissions}'");
            }
            user_data.push_str("    content: |\n");
            for line in file.content.lines() {
                let _ = writeln!(user_data, "      {line}");
            }
        }

        user_data.push_str("runcmd:\n");
        user_data.push_str("  - systemctl daemon-reload\n");
        user_data.push_str("  - grep -qxF /usr/local/bin/intar-shell /etc/shells || echo /usr/local/bin/intar-shell >> /etc/shells\n");
        user_data.push_str("  - systemctl enable intar-agent\n");
        user_data.push_str("  - systemctl start intar-agent\n");
        user_data.push_str("  - |\n");
        user_data.push_str("      if command -v systemctl >/dev/null 2>&1; then\n");
        user_data.push_str("        for unit in");
        for unit in DEFAULT_MASK_UNITS {
            let _ = write!(user_data, " {unit}");
        }
        user_data.push_str(";\n");
        user_data.push_str("        do\n");
        user_data.push_str("          systemctl mask \"$unit\" || true\n");
        user_data.push_str("        done\n");
        user_data.push_str("      fi\n");

        if let Some(runcmd) = &config.runcmd {
            for line in runcmd.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let _ = writeln!(user_data, "  - {trimmed}");
                }
            }
        }

        user_data
    }

    #[must_use]
    pub fn generate_meta_data(&self, instance_id: &str, hostname: &str) -> String {
        format!("instance-id: {instance_id}\nlocal-hostname: {hostname}\n")
    }

    /// Save generated cloud-init data to the logs directory for inspection.
    ///
    /// # Errors
    /// Returns `VmError` when the directory cannot be created or files cannot be written.
    pub fn save_to_logs(
        &self,
        config: &CloudInitConfig,
        hostname: &str,
        logs_dir: &Path,
    ) -> Result<(), VmError> {
        std::fs::create_dir_all(logs_dir)?;

        let user_data = self.generate_user_data(config, hostname);
        let meta_data = self.generate_meta_data(hostname, hostname);

        std::fs::write(logs_dir.join("user-data.yaml"), &user_data)?;
        std::fs::write(logs_dir.join("meta-data.yaml"), &meta_data)?;
        if let Some(network_cfg) = &config.network_config {
            std::fs::write(logs_dir.join("network-config.yaml"), network_cfg)?;
        }

        Ok(())
    }

    /// Write cloud-init user-data/meta-data into an ISO using whichever tool is available.
    ///
    /// # Errors
    /// Returns `VmError` when temporary files cannot be created or no ISO tooling succeeds.
    pub fn create_iso(
        &self,
        config: &CloudInitConfig,
        hostname: &str,
        output_path: &Path,
    ) -> Result<(), VmError> {
        let temp_dir = tempfile::tempdir()
            .map_err(|e| VmError::CloudInit(format!("Failed to create temp dir: {e}")))?;

        let user_data_path = temp_dir.path().join("user-data");
        let meta_data_path = temp_dir.path().join("meta-data");
        let network_config_path = config
            .network_config
            .as_ref()
            .map(|_| temp_dir.path().join("network-config"));

        let user_data = self.generate_user_data(config, hostname);
        let meta_data = self.generate_meta_data(hostname, hostname);

        std::fs::write(&user_data_path, &user_data)?;
        std::fs::write(&meta_data_path, &meta_data)?;
        if let (Some(path), Some(network_cfg)) = (&network_config_path, &config.network_config) {
            std::fs::write(path, network_cfg)?;
        }

        // Try multiple tools in order of preference
        Self::try_cloud_localds(
            output_path,
            &user_data_path,
            &meta_data_path,
            network_config_path.as_deref(),
        )
        .or_else(|_| {
            Self::try_mkisofs(
                output_path,
                &user_data_path,
                &meta_data_path,
                network_config_path.as_deref(),
            )
        })
        .or_else(|_| {
            Self::try_genisoimage(
                output_path,
                &user_data_path,
                &meta_data_path,
                network_config_path.as_deref(),
            )
        })
        .or_else(|_| {
            Self::try_xorriso(
                output_path,
                &user_data_path,
                &meta_data_path,
                network_config_path.as_deref(),
            )
        })
        .or_else(|_| {
            Self::try_hdiutil(
                output_path,
                &user_data_path,
                &meta_data_path,
                network_config_path.as_deref(),
            )
        })
        .map_err(|_| {
            VmError::CloudInit(
                "No ISO creation tool available. Install one of: cloud-localds, mkisofs (brew install cdrtools), genisoimage, or xorriso".into(),
            )
        })
    }

    fn try_cloud_localds(
        output_path: &Path,
        user_data_path: &Path,
        meta_data_path: &Path,
        network_config_path: Option<&Path>,
    ) -> Result<(), VmError> {
        let mut cmd = Command::new("cloud-localds");
        if let Some(network_cfg) = network_config_path {
            cmd.arg(format!("--network-config={}", path_to_str(network_cfg)?));
        }
        let output = cmd
            .arg(output_path)
            .arg(user_data_path)
            .arg(meta_data_path)
            .output()
            .map_err(|e| VmError::CloudInit(e.to_string()))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(VmError::CloudInit(format!(
                "cloud-localds failed: {stderr}"
            )))
        }
    }

    fn try_mkisofs(
        output_path: &Path,
        user_data_path: &Path,
        meta_data_path: &Path,
        network_config_path: Option<&Path>,
    ) -> Result<(), VmError> {
        let output_str = path_to_str(output_path)?;
        let user_data_str = path_to_str(user_data_path)?;
        let meta_data_str = path_to_str(meta_data_path)?;
        let mut args: Vec<String> = vec![
            "-output".into(),
            output_str.into(),
            "-volid".into(),
            "cidata".into(),
            "-joliet".into(),
            "-rock".into(),
            user_data_str.into(),
            meta_data_str.into(),
        ];
        if let Some(network_cfg) = network_config_path {
            args.push(path_to_str(network_cfg)?.into());
        }

        let output = Command::new("mkisofs")
            .args(&args)
            .output()
            .map_err(|e| VmError::CloudInit(e.to_string()))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(VmError::CloudInit(format!("mkisofs failed: {stderr}")))
        }
    }

    fn try_genisoimage(
        output_path: &Path,
        user_data_path: &Path,
        meta_data_path: &Path,
        network_config_path: Option<&Path>,
    ) -> Result<(), VmError> {
        let output_str = path_to_str(output_path)?;
        let user_data_str = path_to_str(user_data_path)?;
        let meta_data_str = path_to_str(meta_data_path)?;
        let mut args: Vec<String> = vec![
            "-output".into(),
            output_str.into(),
            "-volid".into(),
            "cidata".into(),
            "-joliet".into(),
            "-rock".into(),
            user_data_str.into(),
            meta_data_str.into(),
        ];
        if let Some(network_cfg) = network_config_path {
            args.push(path_to_str(network_cfg)?.into());
        }

        let output = Command::new("genisoimage")
            .args(&args)
            .output()
            .map_err(|e| VmError::CloudInit(e.to_string()))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(VmError::CloudInit(format!("genisoimage failed: {stderr}")))
        }
    }

    fn try_xorriso(
        output_path: &Path,
        user_data_path: &Path,
        meta_data_path: &Path,
        network_config_path: Option<&Path>,
    ) -> Result<(), VmError> {
        let output_str = path_to_str(output_path)?;
        let user_data_str = path_to_str(user_data_path)?;
        let meta_data_str = path_to_str(meta_data_path)?;
        let mut args: Vec<String> = vec![
            "-as".into(),
            "mkisofs".into(),
            "-output".into(),
            output_str.into(),
            "-volid".into(),
            "cidata".into(),
            "-joliet".into(),
            "-rock".into(),
            user_data_str.into(),
            meta_data_str.into(),
        ];
        if let Some(network_cfg) = network_config_path {
            args.push(path_to_str(network_cfg)?.into());
        }

        let output = Command::new("xorriso")
            .args(&args)
            .output()
            .map_err(|e| VmError::CloudInit(e.to_string()))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(VmError::CloudInit(format!("xorriso failed: {stderr}")))
        }
    }

    fn try_hdiutil(
        output_path: &Path,
        user_data_path: &Path,
        meta_data_path: &Path,
        network_config_path: Option<&Path>,
    ) -> Result<(), VmError> {
        // hdiutil is macOS-specific, create a temp directory structure first
        let temp_dir = tempfile::tempdir().map_err(|e| VmError::CloudInit(e.to_string()))?;

        let iso_root = temp_dir.path().join("cidata");
        std::fs::create_dir_all(&iso_root)?;

        std::fs::copy(user_data_path, iso_root.join("user-data"))?;
        std::fs::copy(meta_data_path, iso_root.join("meta-data"))?;
        if let Some(network_cfg) = network_config_path {
            std::fs::copy(network_cfg, iso_root.join("network-config"))?;
        }

        let output = Command::new("hdiutil")
            .args(["makehybrid", "-iso", "-joliet", "-o"])
            .arg(output_path)
            .arg(&iso_root)
            .output()
            .map_err(|e| VmError::CloudInit(e.to_string()))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(VmError::CloudInit(format!("hdiutil failed: {stderr}")))
        }
    }
}
