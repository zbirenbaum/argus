// Rust guideline compliant 2026-02-21
//! Async stream sources for non-ptrace pipelines.
//!
//! Each stream polls its data source at a configurable interval and
//! yields items for downstream pipeline stages. Cancellation is via
//! `tokio_util::sync::CancellationToken` — dropping the token or
//! calling `.cancel()` causes the stream to terminate.

pub(crate) mod keylog;
pub(crate) mod proxy;

pub(crate) use keylog::KeylogStream;
pub(crate) use proxy::ProxyStream;
