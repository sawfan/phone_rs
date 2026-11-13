use std::fmt;

/// Why speech could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtsError {
    /// The backend is missing or misconfigured.
    Config(String),
    /// The text or options cannot be spoken.
    Input(String),
    /// The backend failed or answered with something unusable.
    Backend { backend: String, message: String },
}

impl fmt::Display for TtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) | Self::Input(message) => write!(f, "{message}"),
            Self::Backend { backend, message } => write!(f, "{backend}: {message}"),
        }
    }
}

impl std::error::Error for TtsError {}

impl From<TtsError> for String {
    fn from(error: TtsError) -> Self {
        error.to_string()
    }
}
