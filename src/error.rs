use std::fmt;

#[derive(Debug)]
pub enum FetchError {
    InvalidUrl(String),
    Tls(String),
    Io(std::io::Error),
    Http(String),
    Ghost(String),
    Timeout,
    TooManyRedirects,
}

impl FetchError {
    pub fn ghost(msg: impl Into<String>) -> Self {
        Self::Ghost(msg.into())
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(u) => write!(f, "invalid url: {u}"),
            Self::Tls(e) => write!(f, "tls: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Http(e) => write!(f, "http: {e}"),
            Self::Ghost(e) => write!(f, "ghost: {e}"),
            Self::Timeout => write!(f, "timeout"),
            Self::TooManyRedirects => write!(f, "too many redirects"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<std::io::Error> for FetchError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
