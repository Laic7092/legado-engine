//! Errors for the modules that need a typed error instead of `anyhow`.
//!
//! `Internal` keeps the whole anyhow chain (`{0:#}`): the caller shows the
//! message, and a bare "internal error" hides whether it was the network, the
//! site or a script.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Wraps an `anyhow` error, printing its context chain.
    #[error("internal error: {0:#}")]
    Internal(#[from] anyhow::Error),
}
