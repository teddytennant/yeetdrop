//! The Wi-Fi card: which interface carries AWDL, how its driver behaves, and
//! which channel the access point sits on.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::paths;

/// Channels AWDL can use, as the access point's frequency in MHz.
const AWDL_CHANNELS: [(u32, u32); 3] = [(2437, 6), (5220, 44), (5745, 149)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Station {
    pub iface: String,
    pub phy: String,
    pub driver: String,
}

/// How OWL has to be run on one kernel driver. Recorded per driver because
/// mac80211 drivers differ in what survives a monitor interface being added
/// and deleted next to a connected station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub driver: &'static str,
    pub tested: bool,
    /// The monitor interface keeps a MAC of its own and OWL is told the
    /// station's (`owl -m`), instead of the monitor taking the station's MAC.
    pub own_mon_mac: bool,
    /// Force a fresh association after deleting the monitor interface.
    pub reconnect: bool,
    /// Turn mt76 runtime power management off while the window is open.
    pub mt76_pm: bool,
    pub note: &'static str,
}

const PROFILES: &[Profile] = &[Profile {
    driver: "mt7925e",
    tested: true,
    own_mon_mac: false,
    reconnect: true,
    mt76_pm: true,
    note: "MediaTek MT7925: injects while associated; the station stops receiving \
           minutes after a monitor interface is deleted unless it reconnects",
}];

const UNKNOWN: Profile = Profile {
    driver: "*",
    tested: false,
    own_mon_mac: false,
    reconnect: true,
    mt76_pm: false,
    note: "untested card: needs monitor-mode injection while associated",
};

pub fn profile_for(driver: &str) -> Profile {
    PROFILES.iter().copied().find(|p| p.driver == driver).unwrap_or(UNKNOWN)
}

/// Untested cards only run once an administrator opts in.
pub fn allow_untested() -> bool {
    fs::read_to_string(Path::new(paths::CONF).join("allow-untested"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

pub fn runnable(p: &Profile) -> bool {
    p.tested || allow_untested()
}

pub fn awdl_channel(freq_mhz: u32) -> Option<u32> {
    AWDL_CHANNELS.iter().find(|(f, _)| *f == freq_mhz).map(|(_, c)| *c)
}

/// The first managed (station-mode) Wi-Fi interface that isn't ours.
pub fn find_station() -> Option<Station> {
    let out = Command::new("iw").arg("dev").output().ok()?;
    parse_iw_dev(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .find(|s| s.0 != "awdl0" && s.0 != "mon0")
        .map(|(iface, phy)| Station { driver: driver_of(&iface), iface, phy })
}

/// `(interface, phy)` for every managed interface in `iw dev` output.
fn parse_iw_dev(text: &str) -> Vec<(String, String)> {
    let (mut phy, mut iface) = (String::new(), None::<String>);
    let mut found = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("phy#") {
            phy = format!("phy{n}");
        } else if let Some(name) = line.strip_prefix("Interface ") {
            iface = Some(name.to_string());
        } else if line == "type managed" {
            if let Some(i) = iface.take() {
                found.push((i, phy.clone()));
            }
        }
    }
    found
}

fn driver_of(iface: &str) -> String {
    fs::read_link(format!("/sys/class/net/{iface}/device/driver"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// The frequency of the access point the station is associated with.
pub fn link_freq(iface: &str) -> Option<u32> {
    let out = Command::new("iw").args(["dev", iface, "link"]).output().ok()?;
    parse_link_freq(&String::from_utf8_lossy(&out.stdout))
}

fn parse_link_freq(text: &str) -> Option<u32> {
    let rest = text.lines().find_map(|l| l.trim().strip_prefix("freq:"))?;
    // Newer iw prints "5220.0".
    rest.trim().split(['.', ' ']).next()?.parse().ok()
}

pub fn mac_of(iface: &str) -> Option<String> {
    fs::read_to_string(format!("/sys/class/net/{iface}/address")).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iw_dev_skips_monitor_and_finds_phy() {
        let text = "phy#0\n\tInterface mon0\n\t\ttype monitor\n\tInterface wlp194s0\n\t\ttype managed\n";
        assert_eq!(parse_iw_dev(text), vec![("wlp194s0".into(), "phy0".into())]);
    }

    #[test]
    fn link_freq_accepts_old_and_new_iw() {
        assert_eq!(parse_link_freq("Connected to aa\n\tfreq: 5220\n"), Some(5220));
        assert_eq!(parse_link_freq("\tfreq: 2437.0\n"), Some(2437));
        assert_eq!(parse_link_freq("Not connected.\n"), None);
    }

    #[test]
    fn channels_and_profiles() {
        assert_eq!(awdl_channel(5745), Some(149));
        assert_eq!(awdl_channel(2412), None);
        assert!(profile_for("mt7925e").tested);
        assert!(!profile_for("iwlwifi").tested);
    }
}
