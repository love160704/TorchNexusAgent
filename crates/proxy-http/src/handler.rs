use anyhow::{anyhow, bail, Context};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{lookup_host, TcpStream};
use torchnexus_core::config::HttpProxyAuthConfig;
use torchnexus_core::session::EntryType;
use torchnexus_proxy_tcp::context::TcpRuntimeContext;
use torchnexus_proxy_tcp::forward::{
    handle_connected_streams_with_initial_client_data, ConnectedStreamMetadata,
};
use torchnexus_uploader::client::PackageUploader;

const MAX_HEADER_BYTES: usize = 64 * 1024;

pub async fn handle_http_proxy_connection<U>(
    mut client: TcpStream,
    client_addr: SocketAddr,
    runtime: Arc<TcpRuntimeContext<U>>,
    auth: Option<&HttpProxyAuthConfig>,
) -> anyhow::Result<()>
where
    U: PackageUploader,
{
    let request = match read_request_head(&mut client).await {
        Ok(request) => request,
        Err(error) => {
            write_response(&mut client, "400 Bad Request", &[]).await?;
            return Err(error);
        }
    };

    if !is_authorized(&request, auth) {
        write_response(
            &mut client,
            "407 Proxy Authentication Required",
            &[("Proxy-Authenticate", "Basic realm=\"TorchNexus\"")],
        )
        .await?;
        bail!("http proxy authentication failed for {client_addr}");
    }

    if request.method.eq_ignore_ascii_case("CONNECT") {
        let target = match parse_authority(&request.target, 443) {
            Ok(target) => target,
            Err(error) => {
                write_response(&mut client, "400 Bad Request", &[]).await?;
                return Err(error);
            }
        };
        let (target_addr, remote) = match connect_target(&target).await {
            Ok(connected) => connected,
            Err(error) => {
                write_response(&mut client, "502 Bad Gateway", &[]).await?;
                return Err(error);
            }
        };
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        return handle_connected_streams_with_initial_client_data(
            client,
            client_addr,
            remote,
            runtime,
            ConnectedStreamMetadata {
                target_addr,
                target_host: Some(target.host),
                entry_type: EntryType::HttpProxy,
            },
            request.buffered_body,
        )
        .await;
    }

    let (target, origin_form) = match request.http_target() {
        Ok(target) => target,
        Err(error) => {
            write_response(&mut client, "400 Bad Request", &[]).await?;
            return Err(error);
        }
    };
    let (target_addr, remote) = match connect_target(&target).await {
        Ok(connected) => connected,
        Err(error) => {
            write_response(&mut client, "502 Bad Gateway", &[]).await?;
            return Err(error);
        }
    };
    let initial_data = request.forwarded(&origin_form);
    handle_connected_streams_with_initial_client_data(
        client,
        client_addr,
        remote,
        runtime,
        ConnectedStreamMetadata {
            target_addr,
            target_host: Some(target.host),
            entry_type: EntryType::HttpProxy,
        },
        initial_data,
    )
    .await
}

#[derive(Debug)]
struct RequestHead {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    buffered_body: Vec<u8>,
}

impl RequestHead {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn http_target(&self) -> anyhow::Result<(Target, String)> {
        if self
            .target
            .get(..7)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
        {
            let rest = &self.target[7..];
            let (authority, path) = match rest.find(['/', '?']) {
                Some(index) if rest.as_bytes()[index] == b'/' => {
                    (&rest[..index], rest[index..].to_string())
                }
                Some(index) => (&rest[..index], format!("/{}", &rest[index..])),
                None => (rest, "/".to_string()),
            };
            return Ok((parse_authority(authority, 80)?, path));
        }
        if self
            .target
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        {
            bail!("HTTPS proxy requests must use CONNECT");
        }
        let host = self
            .header("Host")
            .context("HTTP proxy request has no Host header")?;
        Ok((parse_authority(host, 80)?, self.target.clone()))
    }

    fn forwarded(&self, origin_form: &str) -> Vec<u8> {
        let mut output =
            format!("{} {} {}\r\n", self.method, origin_form, self.version).into_bytes();
        let connection_headers = self
            .header("Connection")
            .into_iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .collect::<Vec<_>>();
        for (name, value) in &self.headers {
            if name.eq_ignore_ascii_case("Proxy-Authorization")
                || name.eq_ignore_ascii_case("Proxy-Connection")
                || name.eq_ignore_ascii_case("Connection")
                || connection_headers
                    .iter()
                    .any(|connection_header| name.eq_ignore_ascii_case(connection_header))
            {
                continue;
            }
            output.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        output.extend_from_slice(b"Connection: close\r\n");
        output.extend_from_slice(b"\r\n");
        output.extend_from_slice(&self.buffered_body);
        output
    }
}

#[derive(Debug)]
struct Target {
    host: String,
    port: u16,
}

async fn read_request_head(client: &mut TcpStream) -> anyhow::Result<RequestHead> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            bail!("HTTP proxy request headers exceed {MAX_HEADER_BYTES} bytes");
        }
        let mut chunk = [0_u8; 4096];
        let count = client.read(&mut chunk).await?;
        if count == 0 {
            bail!("client disconnected before sending HTTP headers");
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };

    let header_text = std::str::from_utf8(&bytes[..header_end])
        .context("HTTP proxy request headers are not valid UTF-8")?;
    let mut lines = header_text[..header_text.len() - 4].split("\r\n");
    let request_line = lines.next().context("missing HTTP request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().context("missing HTTP method")?.to_string();
    let target = parts
        .next()
        .context("missing HTTP request target")?
        .to_string();
    let version = parts.next().context("missing HTTP version")?.to_string();
    if parts.next().is_some() || !matches!(version.as_str(), "HTTP/1.0" | "HTTP/1.1") {
        bail!("invalid HTTP request line");
    }
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').context("invalid HTTP header")?;
            Ok((name.trim().to_string(), value.trim().to_string()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(RequestHead {
        method,
        target,
        version,
        headers,
        buffered_body: bytes[header_end..].to_vec(),
    })
}

fn is_authorized(request: &RequestHead, auth: Option<&HttpProxyAuthConfig>) -> bool {
    let Some(auth) = auth else { return true };
    let expected = format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", auth.username, auth.password))
    );
    request
        .header("Proxy-Authorization")
        .is_some_and(|actual| actual == expected)
}

fn parse_authority(authority: &str, default_port: u16) -> anyhow::Result<Target> {
    if authority.contains('@') {
        bail!("userinfo is not allowed in proxy targets");
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']').context("invalid IPv6 authority")?;
        let port = suffix
            .strip_prefix(':')
            .map(str::parse)
            .transpose()
            .context("invalid proxy target port")?
            .unwrap_or(default_port);
        (host.to_string(), port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') {
            bail!("IPv6 proxy targets must use brackets");
        }
        (
            host.to_string(),
            port.parse().context("invalid proxy target port")?,
        )
    } else {
        (authority.to_string(), default_port)
    };
    if host.is_empty() {
        bail!("proxy target host is empty");
    }
    Ok(Target { host, port })
}

async fn connect_target(target: &Target) -> anyhow::Result<(SocketAddr, TcpStream)> {
    let mut addresses = lookup_host((target.host.as_str(), target.port))
        .await
        .with_context(|| format!("failed to resolve {}:{}", target.host, target.port))?;
    let mut last_error = None;
    for address in addresses.by_ref() {
        match TcpStream::connect(address).await {
            Ok(stream) => return Ok((address, stream)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(anyhow!(
        "failed to connect {}:{}: {}",
        target.host,
        target.port,
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no resolved addresses".into())
    ))
}

async fn write_response(
    client: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
) -> anyhow::Result<()> {
    let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: 0\r\n");
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    client.write_all(response.as_bytes()).await?;
    Ok(())
}
