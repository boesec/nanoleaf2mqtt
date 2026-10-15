//! Errors of the Nanoleaf client. None of them carries the auth token or a URL containing it.

use std::time::Duration;

use crate::model::OutOfRange;

/// Error talking to a Nanoleaf device.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The HTTP client could not be built.
    #[error("building the HTTP client failed")]
    Setup(#[source] reqwest::Error),
    /// The request failed on the transport level (connect, send, read).
    #[error("{operation}: request failed")]
    Http {
        /// What was attempted, without URL (for example `GET /`).
        operation: &'static str,
        /// The transport error, stripped of its URL.
        #[source]
        source: reqwest::Error,
    },
    /// No answer within the request timeout.
    #[error("{operation}: no answer within {after:?}")]
    Timeout {
        /// What was attempted.
        operation: &'static str,
        /// The timeout that expired.
        after: Duration,
    },
    /// The device answered with an unsuccessful HTTP status.
    #[error("{operation}: device answered HTTP {status}")]
    Status {
        /// What was attempted.
        operation: &'static str,
        /// HTTP status code.
        status: u16,
    },
    /// The device's answer is not what the API documents.
    #[error("{operation}: unexpected answer: {detail}")]
    UnexpectedResponse {
        /// What was attempted.
        operation: &'static str,
        /// What was wrong (never contains body content).
        detail: &'static str,
    },
    /// The command value is outside the range the device reported; nothing was sent.
    #[error(transparent)]
    OutOfRange(#[from] OutOfRange),
    /// The device closed the event stream.
    #[error("the device closed the event stream")]
    StreamClosed,
    /// The device refused pairing because it is not in pairing mode.
    #[error(
        "the device refused pairing (HTTP 403): hold the power button for 5-7 seconds until the \
         lights flash, then run pair again within 30 seconds"
    )]
    PairingRefused,
}

impl Error {
    /// Wraps a transport error, removing the URL (which contains the auth token).
    pub(crate) fn http(operation: &'static str, source: reqwest::Error) -> Self {
        Self::Http {
            operation,
            source: source.without_url(),
        }
    }

    /// The error with all its sources, separated by `: `, for logging.
    pub fn chain(&self) -> String {
        let mut out = self.to_string();
        let mut source = std::error::Error::source(self);
        while let Some(err) = source {
            out.push_str(": ");
            out.push_str(&err.to_string());
            source = err.source();
        }
        out
    }
}
