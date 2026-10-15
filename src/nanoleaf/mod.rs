//! Nanoleaf OpenAPI client: REST, event stream and reconnect handling.
//!
//! The auth token is part of every request path, so request URLs are never logged and never put
//! into errors: transport errors are stripped of their URL before they leave this module.

mod client;
mod error;
pub mod events;
mod parse;
pub mod sse;
mod stream;

pub use client::{Client, DEFAULT_PORT, DEFAULT_REQUEST_TIMEOUT, pair};
pub use error::Error;
pub use parse::{parse_effects_list, parse_root};
pub use stream::{EVENT_IDS, StreamOptions, run, spawn};
