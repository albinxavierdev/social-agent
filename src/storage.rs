//! Object storage for screenshots and approved image assets (spec §6.6, §4).
//! `S3Storage` works with any S3-compatible endpoint (AWS S3, R2, MinIO, Supabase Storage S3 API).

use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

#[async_trait]
pub trait ObjectStorage: Send + Sync {
    /// Store bytes under `key`; returns the CDN-ready public URL.
    async fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str) -> ApiResult<String>;
}

fn check_key(key: &str) -> ApiResult<()> {
    if key.is_empty() || key.starts_with('/') || key.contains("..") || key.contains('\\') {
        return Err(ApiError::internal(format!("unsafe storage key: {key}")));
    }
    Ok(())
}

#[derive(Default)]
pub struct MemoryStorage {
    pub objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
}

#[async_trait]
impl ObjectStorage for MemoryStorage {
    async fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str) -> ApiResult<String> {
        check_key(key)?;
        self.objects.lock().unwrap().insert(key.to_string(), (bytes, content_type.to_string()));
        Ok(format!("https://storage.test/{key}"))
    }
}

/// Dev-only filesystem storage.
pub struct LocalFsStorage {
    pub root: PathBuf,
    pub public_base_url: String,
}

#[async_trait]
impl ObjectStorage for LocalFsStorage {
    async fn put(&self, key: &str, bytes: Vec<u8>, _ct: &str) -> ApiResult<String> {
        check_key(key)?;
        let path = self.root.join(key);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(ApiError::internal)?;
        }
        tokio::fs::write(&path, bytes).await.map_err(ApiError::internal)?;
        Ok(format!("{}/{}", self.public_base_url.trim_end_matches('/'), key))
    }
}

pub struct S3Storage {
    bucket: rusty_s3::Bucket,
    creds: rusty_s3::Credentials,
    public_base_url: String,
    http: reqwest::Client,
}

impl S3Storage {
    pub fn new(
        endpoint: &str,
        region: &str,
        bucket: &str,
        access_key: &str,
        secret_key: &str,
        public_base_url: &str,
        path_style: bool,
    ) -> anyhow::Result<Self> {
        let style = if path_style { rusty_s3::UrlStyle::Path } else { rusty_s3::UrlStyle::VirtualHost };
        Ok(Self {
            bucket: rusty_s3::Bucket::new(endpoint.parse()?, style, bucket.to_string(), region.to_string())?,
            creds: rusty_s3::Credentials::new(access_key, secret_key),
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?,
        })
    }
}

#[async_trait]
impl ObjectStorage for S3Storage {
    async fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str) -> ApiResult<String> {
        use rusty_s3::S3Action;
        check_key(key)?;
        let mut action = self.bucket.put_object(Some(&self.creds), key);
        action.headers_mut().insert("content-type", content_type);
        let url = action.sign(Duration::from_secs(300));
        let resp = self
            .http
            .put(url)
            .header("content-type", content_type)
            .body(bytes)
            .send()
            .await
            .map_err(|e| ApiError::internal(format!("s3 put failed: {}", e.without_url())))?;
        if !resp.status().is_success() {
            return Err(ApiError::internal(format!("s3 put returned {}", resp.status())));
        }
        Ok(format!("{}/{}", self.public_base_url, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_path_traversal_keys() {
        let s = MemoryStorage::default();
        assert!(s.put("../etc/passwd", vec![1], "x").await.is_err());
        assert!(s.put("/abs", vec![1], "x").await.is_err());
        assert!(s.put("t1/b1/img.png", vec![1], "image/png").await.is_ok());
    }
}
