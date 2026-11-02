//! wartales-mp: the co-op helper, running inside the game process.
//!
//! The winmm.dll shim (shim/proxy) links this staticlib and starts it from its
//! worker thread via `wartales_mp_start`. The patcher's C ABI
//! (`wartales_tips_*`, shim/proxy/tips.h) ships in the same library.

pub use wartales_tips;

pub mod log;
pub use log::{rand_bytes, Log};
pub mod conn;

pub mod applog;
pub mod code;
pub mod hlpatch;
pub mod install;
pub mod link;
pub mod master;
pub mod modver;
pub mod nat;
pub mod punch;
pub mod relay;
pub mod sdrbridge;
pub mod sendq;
pub mod uid;
pub mod wsx;
