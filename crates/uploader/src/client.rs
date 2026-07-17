use anyhow::Context;
use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::{fmt, path::Path, time::Duration};
use torchnexus_core::config::UploadConfig;

#[async_trait]
pub trait PackageUploader {
    async fn upload_bundle(&self, bundle: UploadBundle<'_>) -> anyhow::Result<UploadReceipt>;
}

#[derive(Debug, Clone, Deserialize)]
pub struct UploadReceipt {
    pub id: String,
    pub bundle_id: String,
    pub status: String,
}

#[derive(Debug)]
pub struct UploadFailure {
    pub retryable: bool,
    pub status: Option<u16>,
    pub title: Option<String>,
    pub detail: String,
}

impl fmt::Display for UploadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.detail)
    }
}

impl std::error::Error for UploadFailure {}

#[derive(Debug, Clone, Copy)]
pub struct UploadBundle<'a> {
    pub bundle_path: &'a Path,
    pub sha256: &'a str,
}

#[derive(Debug, Clone)]
pub struct HttpPackageUploader {
    client: reqwest::Client,
    endpoint: String,
    username: String,
    password: String,
}

impl HttpPackageUploader {
    pub fn new(endpoint: String, username: String, password: String) -> Self {
        Self::with_timeout(endpoint, username, password, 300)
    }

    pub fn with_timeout(
        endpoint: String,
        username: String,
        password: String,
        request_timeout_seconds: u64,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(request_timeout_seconds))
                .build()
                .expect("HTTP client configuration should be valid"),
            endpoint,
            username,
            password,
        }
    }

    pub fn from_config(config: &UploadConfig) -> Self {
        Self::with_timeout(
            config.endpoint.clone(),
            config.basic_auth.username.clone(),
            config.basic_auth.password.clone(),
            config.request_timeout_seconds,
        )
    }
}

#[derive(Debug, Clone)]
pub enum RuntimeUploader {
    Disabled(AlwaysSucceedUploader),
    Http(HttpPackageUploader),
}

impl RuntimeUploader {
    pub fn from_config(config: &UploadConfig) -> Self {
        if config.enabled {
            Self::Http(HttpPackageUploader::from_config(config))
        } else {
            Self::Disabled(AlwaysSucceedUploader)
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysSucceedUploader;

#[async_trait]
impl PackageUploader for AlwaysSucceedUploader {
    async fn upload_bundle(&self, _bundle: UploadBundle<'_>) -> anyhow::Result<UploadReceipt> {
        Ok(UploadReceipt { id: "disabled".to_string(), bundle_id: "disabled".to_string(), status: "queued".to_string() })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysFailUploader;

#[async_trait]
impl PackageUploader for AlwaysFailUploader {
    async fn upload_bundle(&self, _bundle: UploadBundle<'_>) -> anyhow::Result<UploadReceipt> {
        anyhow::bail!("upload failed")
    }
}

#[async_trait]
impl PackageUploader for HttpPackageUploader {
    async fn upload_bundle(&self, bundle: UploadBundle<'_>) -> anyhow::Result<UploadReceipt> {
        let body = tokio::fs::read(bundle.bundle_path)
            .await
            .with_context(|| format!("failed to read bundle {}", bundle.bundle_path.display()))?;
        let file_name = bundle
            .bundle_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture.tlc.zst")
            .to_string();
        let part = Part::bytes(body)
            .file_name(file_name)
            .mime_str("application/zstd")?;
        let form = Form::new()
            .text("sha256", bundle.sha256.to_string())
            .part("data", part);

        let response = self
            .client
            .post(&self.endpoint)
            .basic_auth(&self.username, Some(&self.password))
            .multipart(form)
            .send()
            .await
            .map_err(|error| UploadFailure { retryable: true, status: None, title: None, detail: format!("failed to upload bundle to {}: {error}", self.endpoint) })?;
        let status = response.status();
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::CREATED {
            let response_text = response.text().await.unwrap_or_default();
            let problem = serde_json::from_str::<ProblemDetail>(&response_text).ok();
            let retryable = status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
            return Err(UploadFailure {
                retryable,
                status: Some(status.as_u16()),
                title: problem.as_ref().map(|value| value.title.clone()),
                detail: problem.map(|value| value.detail).unwrap_or_else(|| response_text.trim().to_string()),
            }.into());
        }
        let receipt = response.json::<UploadReceipt>().await.map_err(|error| UploadFailure {
            retryable: true,
            status: Some(status.as_u16()),
            title: None,
            detail: format!("invalid upload response: {error}"),
        })?;
        if receipt.id.is_empty() || receipt.bundle_id.is_empty() || receipt.status.is_empty() {
            return Err(UploadFailure { retryable: true, status: Some(status.as_u16()), title: None, detail: "invalid upload response: required fields were empty".to_string() }.into());
        }
        Ok(receipt)
    }
}

#[async_trait]
impl PackageUploader for RuntimeUploader {
    async fn upload_bundle(&self, bundle: UploadBundle<'_>) -> anyhow::Result<UploadReceipt> {
        match self {
            Self::Disabled(uploader) => uploader.upload_bundle(bundle).await,
            Self::Http(uploader) => uploader.upload_bundle(bundle).await,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProblemDetail {
    title: String,
    detail: String,
}

#[cfg(test)]
mod tests {
    use super::{HttpPackageUploader, PackageUploader, UploadBundle};
    use anyhow::Context;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct CapturedRequest {
        headers: String,
        body: Vec<u8>,
    }

    fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
        headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then_some(value.trim())
        })
    }

    fn decode_chunked_body(mut bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let size_end = bytes
                .windows(2)
                .position(|window| window == b"\r\n")
                .context("chunked body missing size line terminator")?;
            let size = std::str::from_utf8(&bytes[..size_end])
                .context("chunk size should be utf-8")?
                .trim();
            let size = usize::from_str_radix(size, 16).context("chunk size should be hex")?;
            bytes = &bytes[size_end + 2..];
            if size == 0 {
                break;
            }
            if bytes.len() < size + 2 {
                anyhow::bail!("chunked body ended before declared chunk size");
            }
            body.extend_from_slice(&bytes[..size]);
            bytes = &bytes[size + 2..];
        }
        Ok(body)
    }

    async fn spawn_capture_server() -> (String, tokio::task::JoinHandle<CapturedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("server should bind");
        let addr = listener.local_addr().expect("server should have address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("request should arrive");
            let mut bytes = Vec::new();
            let mut buf = [0_u8; 4096];
            let header_end = loop {
                let n = stream
                    .read(&mut buf)
                    .await
                    .expect("request should be readable");
                assert!(n > 0, "request should include headers");
                bytes.extend_from_slice(&buf[..n]);
                if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8(bytes[..header_end].to_vec())
                .expect("headers should be valid utf-8");
            let body = if let Some(content_length) = header_value(&headers, "content-length") {
                let content_length = content_length
                    .parse::<usize>()
                    .expect("content length should parse");
                while bytes.len() < header_end + content_length {
                    let n = stream
                        .read(&mut buf)
                        .await
                        .expect("request body should be readable");
                    assert!(n > 0, "request body should complete");
                    bytes.extend_from_slice(&buf[..n]);
                }
                bytes[header_end..header_end + content_length].to_vec()
            } else {
                loop {
                    if bytes[header_end..]
                        .windows(5)
                        .any(|window| window == b"0\r\n\r\n")
                    {
                        break;
                    }
                    let n = stream
                        .read(&mut buf)
                        .await
                        .expect("chunked body should be readable");
                    assert!(n > 0, "chunked request body should complete");
                    bytes.extend_from_slice(&buf[..n]);
                }
                decode_chunked_body(&bytes[header_end..]).expect("chunked body should decode")
            };
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("response should be written");

            CapturedRequest { headers, body }
        });

        (format!("http://{addr}/upload"), task)
    }

    #[tokio::test]
    async fn http_uploader_sends_tlc_as_data_part_and_sha256_form_field() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let bundle_path = temp_dir.path().join("bundle.tlc");
        std::fs::write(&bundle_path, b"TLC1 multipart body").expect("bundle should be written");
        let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let (endpoint, request_task) = spawn_capture_server().await;

        let uploader =
            HttpPackageUploader::new(endpoint, "agent".to_string(), "secret".to_string());
        uploader
            .upload_bundle(UploadBundle {
                bundle_path: &bundle_path,
                sha256,
            })
            .await
            .expect("upload should succeed");

        let captured = request_task
            .await
            .context("request task should join")
            .expect("request should be captured");
        let body_text = String::from_utf8(captured.body.clone()).expect("body should be utf-8");
        let content_type = header_value(&captured.headers, "content-type")
            .expect("multipart upload should set content type");

        assert!(captured.headers.contains("POST /upload HTTP/1.1"));
        assert!(content_type.starts_with("multipart/form-data;"));
        assert!(body_text.contains("name=\"sha256\""));
        assert!(body_text.contains(sha256));
        assert!(body_text.contains("name=\"data\"; filename=\"bundle.tlc\""));
        assert!(body_text.contains("Content-Type: application/octet-stream"));
        assert!(captured
            .body
            .windows(b"TLC1 multipart body".len())
            .any(|w| w == b"TLC1 multipart body"));
    }
}
