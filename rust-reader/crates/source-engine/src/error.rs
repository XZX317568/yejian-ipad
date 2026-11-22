//! Public error type for every source-engine operation.

use std::fmt;

/// Every failure the UI has to distinguish while browsing or reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// The `.wasm` module or its metadata could not be loaded.
    Load(String),
    /// The source module does not export the requested function.
    Unimplemented(String),
    /// Transport failure, timeout, invalid URL or TLS setup failure.
    Network(String),
    /// The source failed to parse or query HTML.
    Html(String),
    /// The source needs JavaScript or a web view, which this engine does not run.
    ///
    /// Returned instead of an empty result so the UI can offer an explicit
    /// "open in web view / verify" action.
    WebVerificationRequired {
        detail: String,
    },
    /// The source returned a structured error message.
    Message(String),
    /// Response payload did not match the Aidoku wire format.
    Deserialize(String),
    /// The engine's worker thread stopped or panicked.
    EngineStopped,
    EnginePanicked(String),
    /// A source call consumed more WASM fuel than its per-call budget.
    ///
    /// Returned instead of letting a runaway loop occupy the source thread
    /// forever. `limit` is the configured budget; `0` means unlimited and is
    /// therefore never reported.
    FuelExhausted {
        limit: u64,
    },
    /// The requested chapter is not in the engine's chapter cache. Call
    /// [`crate::SourceEngine::chapters`] first, or use `pages_for`.
    UnknownChapter(String),
    /// Filesystem, archive or metadata failure while managing installed sources.
    Io(String),
    /// The source used `js`/`webview`/canvas functionality that failed natively.
    Unsupported(String),
}

impl SourceError {
    /// Maps the negative status codes defined by the Aidoku WASM ABI.
    pub(crate) fn from_code(code: i32, message: Option<String>) -> Self {
        let msg = message.unwrap_or_default();
        match code {
            -2 => Self::Unimplemented(if msg.is_empty() {
                "source does not implement this function".into()
            } else {
                msg
            }),
            -3 => Self::Network(if msg.is_empty() {
                "network error".into()
            } else {
                msg
            }),
            -4 => Self::Html(if msg.is_empty() {
                "HTML parse/query error".into()
            } else {
                msg
            }),
            -5 => Self::Unsupported(if msg.is_empty() {
                "source requires JavaScript evaluation".into()
            } else {
                msg
            }),
            -6 => Self::Message(if msg.is_empty() {
                "canvas error".into()
            } else {
                msg
            }),
            -7 => Self::Deserialize(if msg.is_empty() {
                "UTF-8 decoding error".into()
            } else {
                msg
            }),
            -8 => Self::Deserialize(if msg.is_empty() {
                "JSON parse error".into()
            } else {
                msg
            }),
            -9 => Self::Deserialize(if msg.is_empty() {
                "deserialization error".into()
            } else {
                msg
            }),
            _ if !msg.is_empty() => Self::Message(msg),
            _ => Self::Message("source returned no result".into()),
        }
    }

    pub fn is_web_verification(&self) -> bool {
        matches!(self, Self::WebVerificationRequired { .. })
    }

    pub(crate) fn web_verification(detail: impl Into<String>) -> Self {
        Self::WebVerificationRequired {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(m) => write!(f, "source load failed: {m}"),
            Self::Unimplemented(m) => write!(f, "source function unimplemented: {m}"),
            Self::Network(m) => write!(f, "network error: {m}"),
            Self::Html(m) => write!(f, "HTML parse/query error: {m}"),
            Self::WebVerificationRequired { detail } => {
                write!(f, "web verification required: {detail}")
            }
            Self::Message(m) => write!(f, "source error: {m}"),
            Self::Deserialize(m) => write!(f, "source response could not be decoded: {m}"),
            Self::EngineStopped => write!(f, "source engine thread is no longer running"),
            Self::EnginePanicked(m) => write!(f, "source engine panicked: {m}"),
            Self::FuelExhausted { limit } => write!(
                f,
                "source call exceeded its WASM fuel budget ({limit} units)"
            ),
            Self::UnknownChapter(id) => write!(
                f,
                "chapter \"{id}\" is not loaded; load the chapter list first"
            ),
            Self::Io(m) => write!(f, "source storage error: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported source feature: {m}"),
        }
    }
}

impl std::error::Error for SourceError {}
