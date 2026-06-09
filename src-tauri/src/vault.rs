//! Re-export of the shared vault logic, which now lives in the `synnotes-core`
//! crate (so the `zen` CLI can share it). Keeps `crate::vault::*` paths working.
pub use synnotes_core::vault::*;
