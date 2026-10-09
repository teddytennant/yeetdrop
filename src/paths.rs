//! Where things live. Root-owned locations are fixed: privileged code must
//! never look anywhere a user can write.

use std::path::PathBuf;

/// Installed runtime: OWL, the AirDrop sender/receiver, the peer-table shim.
pub const LIB: &str = "/usr/lib/yeetdrop";
/// Radio state, recreated on every boot.
pub const RUN: &str = "/run/yeetdrop";
/// Administrator overrides.
pub const CONF: &str = "/etc/yeetdrop";

pub const NM_CONF: &str = "/usr/lib/NetworkManager/conf.d/99-yeetdrop-unmanaged.conf";
pub const POLKIT_POLICY: &str = "/usr/share/polkit-1/actions/io.yeetdrop.peers.policy";

pub fn lib(rel: &str) -> PathBuf {
    PathBuf::from(LIB).join(rel)
}

pub fn run(rel: &str) -> PathBuf {
    PathBuf::from(RUN).join(rel)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Source checkouts and build trees; safe to delete.
pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join("yeetdrop")
}

/// Logs worth keeping after the process exits.
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("yeetdrop")
}
