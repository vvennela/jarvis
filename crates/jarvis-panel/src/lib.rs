//! Bounded file-awareness worker behind the native panel.
#![forbid(unsafe_code)]

#[cfg(unix)]
pub mod worker;
