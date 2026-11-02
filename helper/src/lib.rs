//! wartales-mp: the co-op helper, running inside the game process.
//!
//! The winmm.dll shim (shim/proxy) links this staticlib and starts it from its
//! worker thread via `wartales_mp_start`. The patcher's C ABI
//! (`wartales_tips_*`, shim/proxy/tips.h) ships in the same library.

pub use wartales_tips;
