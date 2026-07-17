use anyhow::Context;
use socks5_proto::{
    handshake::{
        Method as HandshakeMethod, Request as HandshakeRequest, Response as HandshakeResponse,
    },
    Address, Command, Reply, Request, Response,
};
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{lookup_host, TcpStream};
use torchnexus_core::config::Socks5AuthConfig;
use torchnexus_core::session::EntryType;
use torchnexus_proxy_tcp::context::TcpRuntimeContext;
use torchnexus_proxy_tcp::forward::handle_connected_streams;
use torchnexus_uploader::client::PackageUploader;

#[derive(Debug)]
pub struct NegotiatedSocks5Connect {
    pub client: TcpStream,
    pub target_addr: SocketAddr,
    pub target_host: Option<String>,
    pub remote: TcpStream,
}

pub async fn handle_socks5_connection<U>(
    client: TcpStream,
    client_addr: SocketAddr,
    runtime: Arc<TcpRuntimeContext<U>>,
) -> anyhow::Result<()>
where
    U: PackageUploader,
{
    handle_socks5_connection_with_auth(client, client_addr, runtime, None).await
}

pub async fn handle_socks5_connection_with_auth<U>(
    client: TcpStream,
    client_addr: SocketAddr,
    runtime: Arc<TcpRuntimeContext<U>>,
    auth: Option<Socks5AuthConfig>,
) -> anyhow::Result<()>
where
    U: PackageUploader,
{
    let negotiated = negotiate_socks5_connect_with_auth(client, client_addr, auth.as_ref()).await?;
    handle_connected_streams(
        negotiated.client,
        client_addr,
        negotiated.remote,
        runtime,
        negotiated.target_addr,
        negotiated.target_host,
        EntryType::Socks5,
    )
    .await
}

pub async fn negotiate_socks5_connect(
    client: TcpStream,
    client_addr: SocketAddr,
) -> anyhow::Result<NegotiatedSocks5Connect> {
    negotiate_socks5_connect_with_auth(client, client_addr, None).await
}

pub async fn negotiate_socks5_connect_with_auth(
    mut client: TcpStream,
    client_addr: SocketAddr,
    auth: Option<&Socks5AuthConfig>,
) -> anyhow::Result<NegotiatedSocks5Connect> {
    let greeting = read_greeting(&mut client).await?;
    let selected_method = match auth {
        Some(_) if greeting.methods.contains(&HandshakeMethod::PASSWORD) => {
            HandshakeMethod::PASSWORD
        }
        Some(_) => HandshakeMethod::UNACCEPTABLE,
        None if greeting.methods.contains(&HandshakeMethod::NONE) => HandshakeMethod::NONE,
        None => HandshakeMethod::UNACCEPTABLE,
    };
    if selected_method == HandshakeMethod::UNACCEPTABLE {
        HandshakeResponse::new(HandshakeMethod::UNACCEPTABLE)
            .write_to(&mut client)
            .await?;
        anyhow::bail!("no supported auth methods from {client_addr}");
    }
    HandshakeResponse::new(selected_method)
        .write_to(&mut client)
        .await?;
    if let Some(auth) = auth {
        authenticate_password(&mut client, auth).await?;
    }

    let request = match read_request(&mut client).await {
        Ok(request) => request,
        Err(failure) => {
            Response::new(failure.reply, Address::unspecified())
                .write_to(&mut client)
                .await?;
            anyhow::bail!(failure.message);
        }
    };
    let (target_addr, target_host) = match resolve_request_target(request).await {
        Ok(target) => target,
        Err(failure) => {
            Response::new(failure.reply, Address::unspecified())
                .write_to(&mut client)
                .await?;
            anyhow::bail!(failure.message);
        }
    };

    let remote = match TcpStream::connect(target_addr).await {
        Ok(remote) => remote,
        Err(error) => {
            Response::new(Reply::GeneralFailure, Address::unspecified())
                .write_to(&mut client)
                .await?;
            return Err(error).with_context(|| format!("failed to connect remote {target_addr}"));
        }
    };

    Response::new(Reply::Succeeded, Address::unspecified())
        .write_to(&mut client)
        .await?;

    Ok(NegotiatedSocks5Connect {
        client,
        target_addr,
        target_host,
        remote,
    })
}

async fn authenticate_password(
    client: &mut TcpStream,
    auth: &Socks5AuthConfig,
) -> anyhow::Result<()> {
    let mut version_and_length = [0_u8; 2];
    client.read_exact(&mut version_and_length).await?;
    let username_len = version_and_length[1] as usize;
    let mut username = vec![0_u8; username_len];
    client.read_exact(&mut username).await?;
    let mut password_len = [0_u8; 1];
    client.read_exact(&mut password_len).await?;
    let mut password = vec![0_u8; password_len[0] as usize];
    client.read_exact(&mut password).await?;

    let valid = version_and_length[0] == 1
        && username == auth.username.as_bytes()
        && password == auth.password.as_bytes();
    client.write_all(&[1, if valid { 0 } else { 1 }]).await?;
    if !valid {
        anyhow::bail!("invalid SOCKS5 username or password");
    }
    Ok(())
}

async fn read_greeting(client: &mut TcpStream) -> anyhow::Result<HandshakeRequest> {
    HandshakeRequest::read_from(client)
        .await
        .map_err(Into::into)
}

async fn read_request(client: &mut TcpStream) -> Result<Request, RequestFailure> {
    let bytes = read_request_bytes(client).await?;
    let (mut writer, mut reader) = tokio::io::duplex(bytes.len().max(1));
    writer.write_all(&bytes).await.map_err(|error| {
        RequestFailure::general(format!("failed to buffer socks5 request: {error:#}"))
    })?;
    drop(writer);

    Request::read_from(&mut reader)
        .await
        .map_err(|error| match error {
            socks5_proto::Error::Protocol(
                socks5_proto::ProtocolError::InvalidAddressTypeInRequest { address_type, .. },
            ) => RequestFailure::unsupported_address_type(address_type),
            socks5_proto::Error::Protocol(socks5_proto::ProtocolError::InvalidCommand {
                command,
                ..
            }) => RequestFailure::unsupported_command(command),
            error => RequestFailure::general(format!("failed to parse socks5 request: {error:#}")),
        })
}

async fn read_request_bytes(client: &mut TcpStream) -> Result<Vec<u8>, RequestFailure> {
    let mut header = [0_u8; 4];
    client.read_exact(&mut header).await.map_err(|error| {
        RequestFailure::general(format!("failed to read socks5 request header: {error:#}"))
    })?;
    let reserved_byte_error =
        (header[2] != 0).then(|| RequestFailure::invalid_reserved_byte(header[2]));

    let mut bytes = Vec::with_capacity(4 + 1 + 255 + 2);
    bytes.extend_from_slice(&header);
    match header[3] {
        0x01 => {
            let mut body = [0_u8; 6];
            client.read_exact(&mut body).await.map_err(|error| {
                RequestFailure::general(format!(
                    "failed to read socks5 ipv4 request body: {error:#}"
                ))
            })?;
            bytes.extend_from_slice(&body);
        }
        0x03 => {
            let mut length = [0_u8; 1];
            client.read_exact(&mut length).await.map_err(|error| {
                RequestFailure::general(format!("failed to read socks5 domain length: {error:#}"))
            })?;
            bytes.push(length[0]);

            let mut body = vec![0_u8; length[0] as usize + 2];
            client.read_exact(&mut body).await.map_err(|error| {
                RequestFailure::general(format!(
                    "failed to read socks5 domain request body: {error:#}"
                ))
            })?;
            bytes.extend_from_slice(&body);
        }
        0x04 => {
            let mut body = [0_u8; 18];
            client.read_exact(&mut body).await.map_err(|error| {
                RequestFailure::general(format!(
                    "failed to read socks5 ipv6 request body: {error:#}"
                ))
            })?;
            bytes.extend_from_slice(&body);
        }
        other => return Err(RequestFailure::unsupported_address_type(other)),
    }

    if let Some(error) = reserved_byte_error {
        return Err(error);
    }

    Ok(bytes)
}

#[derive(Debug)]
struct RequestFailure {
    reply: Reply,
    message: String,
}

impl RequestFailure {
    fn general(message: String) -> Self {
        Self {
            reply: Reply::GeneralFailure,
            message,
        }
    }

    fn unsupported_command(code: u8) -> Self {
        Self {
            reply: Reply::CommandNotSupported,
            message: format!("unsupported socks5 command {code}"),
        }
    }

    fn unsupported_address_type(atyp: u8) -> Self {
        Self {
            reply: Reply::AddressTypeNotSupported,
            message: format!("unsupported socks5 address type {atyp}"),
        }
    }

    fn invalid_reserved_byte(rsv: u8) -> Self {
        Self {
            reply: Reply::GeneralFailure,
            message: format!("invalid socks5 reserved byte {rsv}"),
        }
    }

    fn invalid_domain_encoding() -> Self {
        Self {
            reply: Reply::GeneralFailure,
            message: "invalid socks5 domain encoding".to_string(),
        }
    }
}

impl fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RequestFailure {}

async fn resolve_request_target(
    request: Request,
) -> Result<(SocketAddr, Option<String>), RequestFailure> {
    match (request.command, request.address) {
        (Command::Connect, Address::SocketAddress(SocketAddr::V4(addr))) => {
            Ok((SocketAddr::V4(addr), None))
        }
        (Command::Connect, Address::SocketAddress(SocketAddr::V6(_))) => {
            Err(RequestFailure::unsupported_address_type(0x04))
        }
        (Command::Connect, Address::DomainAddress(host, port)) => {
            let host =
                String::from_utf8(host).map_err(|_| RequestFailure::invalid_domain_encoding())?;
            let target = {
                let mut resolved = lookup_host((host.as_str(), port)).await.map_err(|error| {
                    RequestFailure::general(format!(
                        "failed to resolve domain {host}:{port}: {error:#}"
                    ))
                })?;
                resolved.find(|addr| addr.is_ipv4()).ok_or_else(|| {
                    RequestFailure::general(format!("domain {host}:{port} did not resolve to ipv4"))
                })?
            };
            Ok((target, Some(host)))
        }
        (Command::Bind, _) => Err(RequestFailure::unsupported_command(u8::from(Command::Bind))),
        (Command::Associate, _) => Err(RequestFailure::unsupported_command(u8::from(
            Command::Associate,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{read_request, resolve_request_target};
    use socks5_proto::{Address, Command, Request};
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};

    async fn connected_streams() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let client = TcpStream::connect(bind).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    async fn read_request_error(request_bytes: &[u8]) -> super::RequestFailure {
        let (mut client, mut server) = connected_streams().await;
        client.write_all(request_bytes).await.unwrap();
        read_request(&mut server)
            .await
            .expect_err("request should fail to decode")
    }

    #[tokio::test]
    async fn read_request_decodes_ipv4_connect_from_socks5_proto() {
        let (mut client, mut server) = connected_streams().await;
        let expected = Request::new(
            Command::Connect,
            Address::SocketAddress("127.0.0.1:9000".parse().unwrap()),
        );
        expected.write_to(&mut client).await.unwrap();

        let actual: Request = read_request(&mut server)
            .await
            .expect("ipv4 connect request should decode from socks5-proto bytes");
        assert_eq!(actual.command, expected.command);
        assert_eq!(actual.address, expected.address);
    }

    #[tokio::test]
    async fn read_request_decodes_domain_connect_from_socks5_proto() {
        let (mut client, mut server) = connected_streams().await;
        let expected = Request::new(
            Command::Connect,
            Address::DomainAddress(b"example.com".to_vec(), 443),
        );
        expected.write_to(&mut client).await.unwrap();

        let actual: Request = read_request(&mut server)
            .await
            .expect("domain connect request should decode from socks5-proto bytes");
        assert_eq!(actual.command, expected.command);
        assert_eq!(actual.address, expected.address);
    }

    #[tokio::test]
    async fn read_request_rejects_non_zero_reserved_byte() {
        let err = read_request_error(&[0x05, 0x01, 0x01, 0x01, 127, 0, 0, 1, 0x23, 0x28]).await;
        assert_eq!(err.reply, socks5_proto::Reply::GeneralFailure);
        assert!(err.message.contains("reserved"));
    }

    #[tokio::test]
    async fn read_request_maps_unknown_command_to_command_not_supported() {
        let err = read_request_error(&[0x05, 0x09, 0x00, 0x01, 127, 0, 0, 1, 0x23, 0x28]).await;
        assert_eq!(err.reply, socks5_proto::Reply::CommandNotSupported);
        assert!(err.message.contains("unsupported socks5 command 9"));
    }

    #[tokio::test]
    async fn read_request_maps_unknown_address_type_to_address_type_not_supported() {
        let err = read_request_error(&[0x05, 0x01, 0x00, 0x09]).await;
        assert_eq!(err.reply, socks5_proto::Reply::AddressTypeNotSupported);
        assert!(err.message.contains("unsupported socks5 address type 9"));
    }

    #[tokio::test]
    async fn resolve_request_target_rejects_invalid_domain_utf8() {
        let err = resolve_request_target(Request::new(
            Command::Connect,
            Address::DomainAddress(vec![0xff, 0xfe], 443),
        ))
        .await
        .expect_err("invalid domain bytes should fail");
        assert_eq!(err.reply, socks5_proto::Reply::GeneralFailure);
        assert!(err.message.contains("invalid socks5 domain encoding"));
    }
}
