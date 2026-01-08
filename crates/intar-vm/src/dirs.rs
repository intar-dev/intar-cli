use crate::VmError;
use std::path::PathBuf;

pub struct IntarDirs {
    pub cache: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
}

impl IntarDirs {
    /// Locate platform-specific cache/state/config directories for intar.
    ///
    /// # Errors
    /// Returns `VmError` if standard OS directories cannot be determined.
    pub fn new() -> Result<Self, VmError> {
        let cache = dirs::cache_dir()
            .ok_or_else(|| VmError::Directory("cache directory not found".into()))?
            .join("intar");

        let state = dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .ok_or_else(|| VmError::Directory("state directory not found".into()))?
            .join("intar");

        let config = dirs::config_dir()
            .ok_or_else(|| VmError::Directory("config directory not found".into()))?
            .join("intar");

        Ok(Self {
            cache,
            state,
            config,
        })
    }

    #[must_use]
    pub fn images_dir(&self) -> PathBuf {
        self.cache.join("images")
    }

    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.state.join("runs")
    }

    #[must_use]
    pub fn new_run_dir(&self) -> PathBuf {
        let name = generate_run_name();
        self.runs_dir().join(name)
    }

    /// Create cache/state/config directories if they do not exist.
    ///
    /// # Errors
    /// Returns an `std::io::Error` if any directory creation fails.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.images_dir())?;
        std::fs::create_dir_all(self.runs_dir())?;
        std::fs::create_dir_all(&self.config)?;
        Ok(())
    }
}

#[must_use]
pub fn generate_run_name() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let suffix: u16 = rng.random_range(1000..9999);

    petname::petname(2, "-").map_or_else(
        || format!("run-{}", rng.random::<u32>()),
        |name| format!("{name}-{suffix}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_run_name() {
        let name = generate_run_name();
        assert!(!name.is_empty());
        assert!(name.contains('-'));
    }

    #[test]
    fn test_intar_dirs() {
        let dirs = IntarDirs::new().unwrap();
        assert!(dirs.images_dir().to_string_lossy().contains("intar"));
        assert!(dirs.runs_dir().to_string_lossy().contains("intar"));
    }
}
