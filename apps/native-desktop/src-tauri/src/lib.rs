#[cfg(feature = "desktop")]
pub mod shell;
pub mod supervisor;

#[cfg(any(feature = "desktop", test))]
mod ui_health;

#[cfg(any(feature = "desktop", test))]
mod background;

#[cfg(target_os = "macos")]
pub mod native_sources;

#[cfg(target_os = "macos")]
pub mod native_control;

pub mod chatlog;

#[cfg(target_os = "macos")]
pub mod collectors;
