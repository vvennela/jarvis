//! Bounded file-awareness worker behind the native panel.
#![forbid(unsafe_code)]

#[cfg(unix)]
pub mod worker;

pub mod chat;

pub mod speech;

pub mod voiceworker;

pub mod wake;
pub mod wakeworker;

pub mod osworker;
pub mod voicehandoff;
