use crate::VmError;
use futures_util::StreamExt;
use intar_core::ImageSource;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

pub struct ImageCache {
    dir: PathBuf,
}

impl ImageCache {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    #[must_use]
    pub fn is_cached(&self, source: &ImageSource) -> bool {
        let filename = Self::cache_filename(&source.url, &source.arch);
        let path = self.dir.join(&filename);
        path.exists()
    }

    #[must_use]
    pub fn get_cached_path(&self, source: &ImageSource) -> Option<PathBuf> {
        let filename = Self::cache_filename(&source.url, &source.arch);
        let path = self.dir.join(&filename);
        if path.exists() { Some(path) } else { None }
    }

    /// Ensure the image is cached locally, downloading if needed.
    ///
    /// # Errors
    /// Returns `VmError` if download or verification fails.
    pub async fn ensure_image(&self, source: &ImageSource) -> Result<PathBuf, VmError> {
        self.ensure_image_with_progress(source, |_| {}).await
    }

    /// Ensure the image is cached locally, emitting progress callbacks while downloading.
    ///
    /// # Errors
    /// Returns `VmError` if download or verification fails.
    pub async fn ensure_image_with_progress<F>(
        &self,
        source: &ImageSource,
        progress_callback: F,
    ) -> Result<PathBuf, VmError>
    where
        F: Fn(f64) + Send + 'static,
    {
        let filename = Self::cache_filename(&source.url, &source.arch);
        let path = self.dir.join(&filename);

        if path.exists() {
            info!("Image already cached: {}", path.display());
            progress_callback(1.0);
            self.verify_checksum(&path, &source.checksum).await?;
            return Ok(path);
        }

        std::fs::create_dir_all(&self.dir)?;

        info!("Downloading image from {}", source.url);
        self.download_with_progress(&source.url, &path, progress_callback)
            .await?;

        self.verify_checksum(&path, &source.checksum).await?;

        Ok(path)
    }

    fn cache_filename(url: &str, arch: &str) -> String {
        let url_hash = {
            let mut hasher = Sha256::new();
            hasher.update(url.as_bytes());
            let result = hasher.finalize();
            hex::encode(&result[..8])
        };

        let basename = url
            .rsplit('/')
            .next()
            .unwrap_or("image")
            .trim_end_matches(".qcow2")
            .trim_end_matches(".img");

        format!("{basename}-{arch}-{url_hash}.img")
    }

    async fn download_with_progress<F>(
        &self,
        url: &str,
        dest: &Path,
        progress_callback: F,
    ) -> Result<(), VmError>
    where
        F: Fn(f64),
    {
        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| VmError::CloudInit(format!("Failed to start download: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(VmError::CloudInit(format!(
                "Download failed with status: {status}"
            )));
        }

        let total_size = response.content_length().unwrap_or(0);

        let temp_path = dest.with_extension("img.tmp");
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(VmError::Io)?;

        let mut downloaded: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| VmError::CloudInit(format!("Download error: {e}")))?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;

            if total_size > 0 {
                let scaled = (u128::from(downloaded) * 10_000u128) / u128::from(total_size);
                let scaled = scaled.min(10_000u128);
                let scaled = u32::try_from(scaled).unwrap_or(10_000);
                let progress = f64::from(scaled) / 10_000.0;
                progress_callback(progress);
            }
        }

        file.flush().await?;
        drop(file);

        progress_callback(1.0);

        tokio::fs::rename(&temp_path, dest).await?;

        info!("Image downloaded to {}", dest.display());
        Ok(())
    }

    async fn verify_checksum(&self, path: &Path, expected: &str) -> Result<(), VmError> {
        let Some(expected_hash) = expected.strip_prefix("sha256:") else {
            warn!("Unknown checksum format, skipping verification");
            return Ok(());
        };

        info!("Verifying checksum for {}", path.display());

        let data = tokio::fs::read(path).await?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let result = hasher.finalize();
        let actual_hash = hex::encode(result);

        if actual_hash != expected_hash {
            return Err(VmError::CloudInit(format!(
                "Checksum mismatch: expected {expected_hash}, got {actual_hash}"
            )));
        }

        info!("Checksum verified");
        Ok(())
    }

    /// List cached image files.
    ///
    /// # Errors
    /// Returns `VmError` if the cache directory cannot be read.
    pub fn list_cached_images(&self) -> Result<Vec<PathBuf>, VmError> {
        let mut images = Vec::new();

        if !self.dir.exists() {
            return Ok(images);
        }

        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext == "img" || ext == "qcow2")
            {
                images.push(path);
            }
        }

        Ok(images)
    }
}
