//! wartales-mp: the whole winmm.dll the game loads from its folder. It forwards
//! every winmm export to System32 (`forward`) and runs the co-op helper inside
//! the game process.

pub use wartales_tips;

pub mod log;
pub use log::{rand_bytes, Log};
pub mod conn;
pub mod dll;
pub mod forward;
pub mod pe;

pub mod applog;
pub mod code;
pub mod hlpatch;
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
