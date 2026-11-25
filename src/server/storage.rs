//! Object storage: any S3-compatible bucket, or a local directory.
//!
//! S3 requests are presigned with `rusty-s3` and sent with `reqwest`, which
//! avoids pulling in the full AWS SDK.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle, actions::ListObjectsV2};

use super::config::StorageConfig;

/// Lifetime of the presigned URLs ansel uses internally.
const PRESIGN_TTL: Duration = Duration::from_secs(60);

pub enum Storage {
    S3 {
        bucket: Bucket,
        credentials: Credentials,
        http: reqwest::Client,
    },
    Fs {
        root: PathBuf,
    },
}

impl Storage {
    pub fn new(config: &StorageConfig) -> Result<Self, String> {
        match config {
            StorageConfig::Fs { root } => Ok(Self::Fs { root: root.clone() }),
            StorageConfig::S3 {
                endpoint,
                bucket,
                region,
                access_key,
                secret_key,
                path_style,
            } => {
                let style = if *path_style {
                    UrlStyle::Path
                } else {
                    UrlStyle::VirtualHost
                };
                let bucket = Bucket::new(endpoint.clone(), style, bucket.clone(), region.clone())
                    .map_err(|e| format!("invalid bucket configuration: {e}"))?;
                let http = reqwest::Client::builder()
                    .timeout(Duration::from_secs(60))
                    .build()
                    .map_err(|e| format!("couldn't build the HTTP client: {e}"))?;
                Ok(Self::S3 {
                    bucket,
                    credentials: Credentials::new(access_key.clone(), secret_key.clone()),
                    http,
                })
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::S3 { bucket, .. } => format!("s3 bucket `{}`", bucket.name()),
            Self::Fs { root } => format!("local directory `{}`", root.display()),
        }
    }

    /// Reads an object; `None` when it doesn't exist.
    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::S3 {
                bucket,
                credentials,
                http,
            } => {
                let url = bucket.get_object(Some(credentials), key).sign(PRESIGN_TTL);
                let res = http.get(url).send().await.map_err(|e| e.to_string())?;
                match res.status() {
                    s if s.is_success() => {
                        Ok(Some(res.bytes().await.map_err(|e| e.to_string())?.to_vec()))
                    }
                    reqwest::StatusCode::NOT_FOUND => Ok(None),
                    s => Err(format!("GET {key} returned {s}")),
                }
            }
            Self::Fs { root } => match tokio::fs::read(root.join(key)).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("read {key}: {e}")),
            },
        }
    }

    pub async fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<(), String> {
        match self {
            Self::S3 {
                bucket,
                credentials,
                http,
            } => {
                let url = bucket.put_object(Some(credentials), key).sign(PRESIGN_TTL);
                let res = http
                    .put(url)
                    .header("content-type", content_type)
                    .body(body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                if res.status().is_success() {
                    Ok(())
                } else {
                    Err(format!("PUT {key} returned {}", res.status()))
                }
            }
            Self::Fs { root } => {
                let path = root.join(key);
                if let Some(dir) = path.parent() {
                    tokio::fs::create_dir_all(dir)
                        .await
                        .map_err(|e| format!("mkdir {key}: {e}"))?;
                }
                // Write to a temporary file and rename, so readers never see
                // a half-written object.
                let tmp = tmp_path(&path);
                tokio::fs::write(&tmp, body)
                    .await
                    .map_err(|e| format!("write {key}: {e}"))?;
                tokio::fs::rename(&tmp, &path)
                    .await
                    .map_err(|e| format!("rename {key}: {e}"))
            }
        }
    }

    /// Deletes every object whose key starts with `prefix`.
    pub async fn delete_prefix(&self, prefix: &str) -> Result<(), String> {
        match self {
            Self::S3 {
                bucket,
                credentials,
                http,
            } => {
                let mut token: Option<String> = None;
                loop {
                    let mut list = bucket.list_objects_v2(Some(credentials));
                    list.with_prefix(prefix);
                    if let Some(t) = &token {
                        list.with_continuation_token(t.as_str());
                    }
                    let res = http
                        .get(list.sign(PRESIGN_TTL))
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    if !res.status().is_success() {
                        return Err(format!("LIST {prefix} returned {}", res.status()));
                    }
                    let text = res.text().await.map_err(|e| e.to_string())?;
                    let page = ListObjectsV2::parse_response(&text)
                        .map_err(|e| format!("LIST {prefix}: {e}"))?;
                    for object in &page.contents {
                        let url = bucket
                            .delete_object(Some(credentials), &object.key)
                            .sign(PRESIGN_TTL);
                        let res = http.delete(url).send().await.map_err(|e| e.to_string())?;
                        if !res.status().is_success()
                            && res.status() != reqwest::StatusCode::NOT_FOUND
                        {
                            return Err(format!("DELETE {} returned {}", object.key, res.status()));
                        }
                    }
                    match page.next_continuation_token {
                        Some(next) => token = Some(next),
                        None => return Ok(()),
                    }
                }
            }
            Self::Fs { root } => match tokio::fs::remove_dir_all(root.join(prefix)).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("delete {prefix}: {e}")),
            },
        }
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    // Random suffix: two requests may render the same variant at once.
    name.push(format!(
        ".tmp-{:016x}",
        getrandom::u64().unwrap_or_default()
    ));
    path.with_file_name(name)
}
