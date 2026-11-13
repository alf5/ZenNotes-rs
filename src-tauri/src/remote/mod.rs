//! Remote workspaces: a vault on a ZenNotes server speaking upstream's
//! `self-hosted-http-v1` protocol (upstream's `znserver`, or a server built on
//! it). See docs/remote-workspace-plan.md.
//!
//! - `profiles`: saved remote workspaces in the config file
//! - `session`: the live connection, app-wide
//! - `http`: the authenticated transport; the only way a request reaches a
//!   server, and the only holder of the token
//! - `watch`: the live change feed
//! - `tls`: the trust store both use

pub mod http;
pub mod profiles;
pub mod session;
mod tls;
pub mod watch;

pub use http::RemoteError;
pub use session::RemoteSession;
