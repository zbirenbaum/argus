//! TLS environment, CA generation, MITM proxy management, and content capture.
//!
//! Implements startup steps 4-6 from the supervisor spec: generating a
//! self-signed CA keypair for TLS interception, launching `mitmdump` as
//! a regular HTTP/HTTPS proxy, and assembling the environment variables
//! that make the traced agent route traffic through the proxy.
//!
//! Also provides TLS content capture: parsing SSLKEYLOGFILE lines, parsing
//! mitmdump flow JSON output, and deduplicating network events.

mod ca;
mod dedup;
mod env;
mod flow_parser;
mod flow_watcher;
mod keylog;
mod mitmdump;

// Cross-crate: supervisor startup needs these directly.
pub use ca::{CaPaths, generate_ca};
pub use env::agent_env_vars;
pub use mitmdump::{AddonConfig, MitmdumpHandle, start_mitmdump, start_mitmdump_with_flow_capture};

// Crate-internal: used only by runtime TLS watcher.
pub(crate) use dedup::NetworkDedup;
pub(crate) use flow_parser::{
    MitmdumpFlow, ProcessedFlow, parse_flow_line, parse_flow_lines, process_flow,
};
pub(crate) use flow_watcher::{FlowEvents, FlowWatcher};
pub(crate) use keylog::{KeylogLine, KeylogWatcher, parse_keylog_line};
