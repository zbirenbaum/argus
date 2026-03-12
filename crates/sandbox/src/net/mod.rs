//! TLS environment, CA generation, and MITM proxy management.
//!
//! Implements startup steps 4-6 from the supervisor spec: generating a
//! self-signed CA keypair for TLS interception, launching `mitmdump` as
//! a regular HTTP/HTTPS proxy, and assembling the environment variables
//! that make the traced agent route traffic through the proxy.

mod ca;
mod env;
mod mitmdump;

pub use ca::{CaPaths, generate_ca};
pub use env::agent_env_vars;
pub use mitmdump::{MitmdumpHandle, start_mitmdump};
