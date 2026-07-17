use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct NewSessionInfo {
    pub client_addr: SocketAddr,
    pub target_addr: SocketAddr,
    pub target_host: Option<String>,
    pub entry_type: EntryType,
    pub capture_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    TcpForward,
    Socks5,
    HttpProxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosedReason {
    TcpDisconnect,
    AgentShutdown,
    ConnectFailed,
    ForwardError,
}
