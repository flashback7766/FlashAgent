//! flashagent-svc: the self-updater and the uninstaller.

pub mod uninstall;
pub mod updater;
pub mod version;

pub use version::Version;
