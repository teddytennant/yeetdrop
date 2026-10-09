//! `doctor`: what works, what doesn't, and the command that fixes each.

use std::path::Path;
use std::process::{Command, Stdio};

use console::style;

use crate::{hw, paths, radio};

enum Level { Pass, Warn, Fail }

struct Check {
    level: Level,
    what: &'static str,
    detail: String,
    fix: Option<&'static str>,
}

pub struct Report {
    pub failed: usize,
    pub warned: usize,
}

fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

fn checks() -> Vec<Check> {
    use Level::*;
    let mut v = Vec::new();
    let mut add = |level, what, detail: String, fix| v.push(Check { level, what, detail, fix });

    let tools: Vec<&str> = ["iw", "ip", "nmcli", "ping", "python3"].into_iter().filter(|t| !which(t)).collect();
    if tools.is_empty() {
        add(Pass, "Tools", "iw, ip, nmcli, ping, python3".into(), None);
    } else {
        add(Fail, "Tools", format!("missing: {}", tools.join(", ")), Some("yeetdrop setup"));
    }

    match hw::find_station() {
        None => add(Fail, "Wi-Fi card", "no managed Wi-Fi interface".into(), None),
        Some(s) => {
            let p = hw::profile_for(&s.driver);
            if p.tested {
                add(Pass, "Wi-Fi card", format!("{} ({}): tested", s.iface, s.driver), None);
            } else if hw::allow_untested() {
                add(Warn, "Wi-Fi card", format!("{} ({}): untested, allowed", s.iface, s.driver), None);
            } else {
                add(Warn, "Wi-Fi card", format!("{} ({}): untested", s.iface, s.driver), Some("yeetdrop setup --allow-untested"));
            }
            match radio::preflight() {
                Ok((_, _, ch)) => add(Pass, "Access point", format!("channel {ch}, shared with AWDL"), None),
                Err(e) => add(Fail, "Access point", e, Some("set your router to channel 6, 44 or 149")),
            }
        }
    }

    let installed = ["owl", "send-to-peer", "receiver/bin/airdrop-serve.py", "awdl-airdrop-adv.py"]
        .into_iter()
        .filter(|f| !paths::lib(f).exists())
        .collect::<Vec<_>>();
    if installed.is_empty() {
        let ver = std::fs::read_to_string(paths::lib("VERSION")).unwrap_or_default();
        add(Pass, "Installed", format!("{} v{}", paths::LIB, ver.trim()), None);
    } else {
        add(Fail, "Installed", format!("missing {}", installed.join(", ")), Some("yeetdrop setup"));
    }

    let py = Command::new("python3")
        .args(["-I", "-c", "import libarchive, PIL, zeroconf, ifaddr, dbus, gi"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if py {
        add(Pass, "Python modules", "libarchive, PIL, zeroconf, ifaddr, dbus, gi".into(), None);
    } else {
        add(Fail, "Python modules", "one or more can't be imported".into(), Some("yeetdrop setup"));
    }

    let bt = Command::new("systemctl").args(["is-active", "--quiet", "bluetooth"]).status().map(|s| s.success()).unwrap_or(false);
    if bt {
        add(Pass, "Bluetooth", "bluetoothd running (wakes iPhones)".into(), None);
    } else {
        add(Warn, "Bluetooth", "bluetoothd isn't running; iPhones won't be woken".into(), Some("sudo systemctl enable --now bluetooth"));
    }

    if Path::new(paths::NM_CONF).exists() {
        add(Pass, "NetworkManager", "leaves awdl0 and mon0 alone".into(), None);
    } else {
        add(Warn, "NetworkManager", "isn't told to ignore awdl0/mon0".into(), Some("yeetdrop setup"));
    }

    let st = radio::status();
    if st.visible {
        add(Pass, "Window", format!("open, {} device(s) nearby", st.peers), None);
    } else {
        add(Pass, "Window", "closed".into(), None);
    }
    v
}

pub fn run(json: bool) -> Report {
    let list = checks();
    let (mut failed, mut warned) = (0, 0);
    for c in &list {
        let mark = match c.level {
            Level::Pass => crate::ui::ok_mark(),
            Level::Warn => { warned += 1; crate::ui::warn_mark() }
            Level::Fail => { failed += 1; crate::ui::fail_mark() }
        };
        println!("{mark} {:<16} {}", style(c.what).bold(), c.detail);
        if let (Some(fix), false) = (c.fix, matches!(c.level, Level::Pass)) {
            println!("  {:<16} {} {}", "", style("fix:").dim(), style(fix).cyan());
        }
    }
    let _ = json;
    Report { failed, warned }
}
