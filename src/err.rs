//! Petite erreur maison, sans dépendance externe.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum RsError {
    Io(io::Error),
    Msg(String),
}

pub type Result<T> = std::result::Result<T, RsError>;

/// raccourci pour construire une erreur texte
pub fn msg<S: Into<String>>(s: S) -> RsError {
    RsError::Msg(s.into())
}

impl fmt::Display for RsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RsError::Io(e) => write!(f, "erreur système: {e}"),
            RsError::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl From<io::Error> for RsError {
    fn from(e: io::Error) -> RsError {
        RsError::Io(e)
    }
}

impl From<String> for RsError {
    fn from(e: String) -> RsError {
        RsError::Msg(e)
    }
}

impl From<&str> for RsError {
    fn from(e: &str) -> RsError {
        RsError::Msg(e.to_string())
    }
}
