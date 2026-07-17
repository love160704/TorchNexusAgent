#[derive(thiserror::Error, Debug)]
pub enum AgentError {
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("invalid socks5 request")]
    InvalidSocks5Request,
    #[error("unsupported socks5 command: {0}")]
    UnsupportedSocks5Command(u8),
    #[error("upload interval must be at least 60 seconds")]
    UploadIntervalTooSmall,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
