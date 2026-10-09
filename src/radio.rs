//! The radio: AWDL in userspace through OWL on a monitor interface next to the
//! connected Wi-Fi station.
//!
//! `start` (root) builds the interfaces, launches OWL and hands the window to a
//! small daemon. The daemon announces `_airdrop._tcp`, watches OWL, ends the
//! window at expiry and is the only place teardown happens: `stop` just asks it
//! to finish, so there is one exit path whatever ends the window.
//!
//! Everything under /run/yeetdrop is world-readable so `status` and `peers`
//! never need privilege.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::hw::{self, Profile};
use crate::paths;

const AWDL: &str = "awdl0";
const MON: &str = "mon0";
const ADV_PORT: &str = "8771";
/// Receiver capability flags in the announcement: SUPPORTS_MIXED_TYPES |
/// SUPPORTS_DISCOVER_MAYBE (0x88, what OpenDrop sends) | SUPPORTS_URL.
const ADV_FLAGS: &str = "137";
const ADV_INTERVAL: Duration = Duration::from_secs(3);

/// An error that carries the helper-contract exit code.
#[derive(Debug)]
pub struct RadioError {
    pub code: i32,
    pub msg: String,
}

impl std::fmt::Display for RadioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for RadioError {}

/// 2: usage/not root. 3: not possible right now (retryable). 4: no AWDL radio.
fn fail<T>(code: i32, msg: impl Into<String>) -> Result<T> {
    Err(RadioError { code, msg: msg.into() }.into())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct State {
    station: String,
    channel: u32,
    /// Unix seconds when the window ends; 0 means until stopped.
    until: u64,
    owl_pid: i32,
    mt76_pm: Option<(String, String)>,
    reconnect: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct Status {
    pub visible: bool,
    pub remaining: Option<u64>,
    pub channel: Option<u32>,
    pub station: Option<String>,
    pub peers: usize,
    pub reason: Option<String>,
}

pub enum Started {
    Fresh,
    Extended,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn until_for(secs: u64) -> u64 {
    if secs == 0 { 0 } else { now() + secs }
}

fn read_state() -> Option<State> {
    serde_json::from_slice(&fs::read(paths::run("state.json")).ok()?).ok()
}

fn write_state(st: &State) -> Result<()> {
    let tmp = paths::run("state.json.tmp");
    fs::write(&tmp, serde_json::to_vec(st)?)?;
    fs::rename(&tmp, paths::run("state.json"))?;
    Ok(())
}

/// Through /proc rather than `kill -0`: unprivileged callers get EPERM from the
/// latter for root's processes. The command line must match too, so a recycled
/// pid doesn't pass.
fn pid_matches(pid: i32, needle: &str) -> bool {
    pid > 0
        && fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| String::from_utf8_lossy(&b).replace('\0', " ").contains(needle))
            .unwrap_or(false)
}

fn daemon_pid() -> i32 {
    fs::read_to_string(paths::run("daemon.pid")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

fn daemon_alive() -> bool {
    pid_matches(daemon_pid(), "radio daemon")
}

fn owl_path() -> String {
    paths::lib("owl").display().to_string()
}

/// Run a command, ignoring failure. Teardown must not stop at the first
/// step that has nothing left to do.
fn quiet(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn log(msg: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(paths::run("log")) {
        let _ = writeln!(f, "{} {msg}", now());
    }
}

fn need_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return fail(2, "this needs root; run it through `yeetdrop on`");
    }
    Ok(())
}

// ------------------------------------------------------------------- status

/// awdl0 has a usable IPv6 link-local: omdrop's receiver can't bind without one.
fn link_local_ready() -> bool {
    let text = out("ip", &["-6", "addr", "show", "dev", AWDL, "scope", "link"]);
    text.contains("inet6") && !text.contains("tentative")
}

pub fn status() -> Status {
    let last = fs::read_to_string(paths::run("last")).ok().map(|s| s.trim().to_string());
    let Some(st) = read_state() else {
        return Status { reason: last, ..Status::default() };
    };
    let visible = daemon_alive() && pid_matches(st.owl_pid, &owl_path()) && link_local_ready();
    let remaining = (visible && st.until > 0).then(|| st.until.saturating_sub(now()));
    Status {
        visible,
        remaining,
        channel: Some(st.channel),
        station: Some(st.station),
        peers: if visible { peers().len() } else { 0 },
        reason: last,
    }
}

// -------------------------------------------------------------------- peers

/// OWL logs `add peer <mac> (<name>)` and `remove peer <mac> (<name>)` with
/// unpadded octets (`86:8:4:8a:2b:f4`). iPhones randomise their AWDL address,
/// so nothing here remembers a peer past its window.
pub fn parse_peers(log: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for line in log.lines() {
        let (add, rest) = match (line.find("add peer "), line.find("remove peer ")) {
            (Some(i), _) => (true, &line[i + "add peer ".len()..]),
            (None, Some(i)) => (false, &line[i + "remove peer ".len()..]),
            _ => continue,
        };
        let Some(mac) = rest.split_whitespace().next().and_then(normalize_mac) else { continue };
        if add { seen.insert(mac); } else { seen.remove(&mac); }
    }
    seen.into_iter().collect()
}

fn normalize_mac(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 6 || parts.iter().any(|p| p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_hexdigit())) {
        return None;
    }
    Some(parts.iter().map(|p| format!("{:0>2}", p.to_ascii_lowercase())).collect::<Vec<_>>().join(":"))
}

pub fn peers() -> Vec<String> {
    fs::read_to_string(paths::run("owl.log")).map(|l| parse_peers(&l)).unwrap_or_default()
}

// -------------------------------------------------------------------- start

pub fn start(secs: u64) -> Result<Started> {
    need_root()?;
    fs::create_dir_all(paths::RUN)?;
    let lock = File::create(paths::run("lock"))?;
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };

    let owl = paths::lib("owl");
    if !owl.exists() {
        return fail(4, format!("OWL is not installed at {}; run `yeetdrop setup`", owl.display()));
    }

    if daemon_alive() {
        if let Some(mut st) = read_state() {
            st.until = until_for(secs);
            write_state(&st)?;
            log(&format!("already discoverable; window now {secs}s"));
            return Ok(Started::Extended);
        }
    }
    // A previous run that died without tearing down leaves interfaces behind.
    if let Some(stale) = read_state() {
        teardown(&stale, "stale");
    }
    let _ = fs::remove_file(paths::run("log"));
    let _ = fs::remove_file(paths::run("announce.log"));
    let _ = fs::remove_file(paths::run("last"));

    let Some(sta) = hw::find_station() else {
        return fail(4, "no Wi-Fi interface to run AWDL on");
    };
    let profile = hw::profile_for(&sta.driver);
    if !hw::runnable(&profile) {
        return fail(4, format!(
            "yeetdrop hasn't been tested on this Wi-Fi driver ({}). To try anyway: \
             echo 1 | sudo tee {}/allow-untested",
            if sta.driver.is_empty() { "unknown" } else { &sta.driver },
            paths::CONF
        ));
    }

    let Some(gw) = wait_for_gateway(&sta.iface) else {
        return fail(3, "Wi-Fi isn't connected (or isn't passing traffic), and AirDrop shares its channel");
    };
    let freq = hw::link_freq(&sta.iface);
    let Some(channel) = freq.and_then(hw::awdl_channel) else {
        return fail(3, match freq {
            Some(f) => format!("your Wi-Fi network is on {f} MHz, but AirDrop can only share channel 6, 44 or 149 with it"),
            None => "Wi-Fi isn't associated with an access point".into(),
        });
    };
    let sta_mac = hw::mac_of(&sta.iface).context("reading the station's MAC")?;
    log(&format!("station {} ({sta_mac}, {}) on ch{channel}, gateway {gw}", sta.iface, sta.driver));

    quiet("iw", &["dev", MON, "del"]);
    if !quiet("iw", &["phy", &sta.phy, "interface", "add", MON, "type", "monitor"]) {
        return fail(4, format!("could not create the monitor interface {MON}"));
    }
    let (mon_mac, owl_m) = if profile.own_mon_mac {
        (format!("02:{}", sta_mac.split_once(':').map_or("", |x| x.1)), Some(sta_mac.clone()))
    } else {
        (sta_mac.clone(), None)
    };
    quiet("ip", &["link", "set", MON, "address", &mon_mac]);
    if !quiet("ip", &["link", "set", MON, "up"]) {
        quiet("iw", &["dev", MON, "del"]);
        return fail(4, format!("could not bring {MON} up"));
    }

    let pm = if profile.mt76_pm { mt76_pm_off(&sta.phy) } else { None };

    let owl_log = File::create(paths::run("owl.log"))?;
    let mut cmd = Command::new(&owl);
    cmd.args(["-i", MON, "-c", &channel.to_string(), "-N", "-S", "intersect"]);
    if let Some(m) = &owl_m {
        cmd.args(["-m", m]);
    }
    cmd.arg("-v")
        .stdin(Stdio::null())
        .stdout(owl_log.try_clone()?)
        .stderr(owl_log)
        .process_group(0);
    let child = cmd.spawn().context("starting OWL")?;

    let st = State {
        station: sta.iface.clone(),
        channel,
        until: until_for(secs),
        owl_pid: child.id() as i32,
        mt76_pm: pm,
        reconnect: profile.reconnect,
    };
    write_state(&st)?;

    if !wait_until(Duration::from_secs(10), || link_exists(AWDL)) {
        let why = fs::read_to_string(paths::run("owl.log")).ok().and_then(|l| l.lines().last().map(String::from));
        teardown(&st, "owl-failed");
        return fail(4, format!("AWDL didn't come up: {}", why.unwrap_or_else(|| "OWL exited".into())));
    }
    quiet("nmcli", &["device", "set", AWDL, "managed", "no"]);
    quiet("ip", &["link", "set", AWDL, "up"]);
    // Duplicate address detection: the receiver refuses to start without a
    // usable link-local.
    wait_until(Duration::from_secs(10), link_local_ready);

    let daemon = Command::new(std::env::current_exe()?)
        .args(["radio", "daemon"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("starting the radio daemon")?;
    fs::write(paths::run("daemon.pid"), daemon.id().to_string())?;
    log(&format!("discoverable for {secs}s (0 = until stopped)"));
    drop(lock);
    Ok(Started::Fresh)
}

fn link_exists(iface: &str) -> bool {
    std::path::Path::new(&format!("/sys/class/net/{iface}")).exists()
}

fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        sleep(Duration::from_millis(250));
    }
}

/// A freshly reloaded driver takes seconds to associate and get an address;
/// don't judge a link that's still coming up.
fn wait_for_gateway(iface: &str) -> Option<String> {
    for _ in 0..15 {
        let routes = out("ip", &["-4", "route", "show", "default", "dev", iface]);
        let gw = routes.split_whitespace().skip_while(|w| *w != "via").nth(1).map(String::from);
        if let Some(gw) = gw {
            if quiet("ping", &["-c1", "-W1", "-I", iface, &gw]) {
                return Some(gw);
            }
        }
        sleep(Duration::from_secs(1));
    }
    None
}

fn mt76_dir(phy: &str) -> PathBuf {
    PathBuf::from(format!("/sys/kernel/debug/ieee80211/{phy}/mt76"))
}

/// Runtime PM off for the window; returns the previous values for teardown.
fn mt76_pm_off(phy: &str) -> Option<(String, String)> {
    if !std::path::Path::new("/sys/kernel/debug/ieee80211").exists() {
        quiet("mount", &["-t", "debugfs", "none", "/sys/kernel/debug"]);
    }
    let dir = mt76_dir(phy);
    let prev = (
        fs::read_to_string(dir.join("runtime-pm")).ok()?.trim().to_string(),
        fs::read_to_string(dir.join("deep-sleep")).ok()?.trim().to_string(),
    );
    fs::write(dir.join("runtime-pm"), "0").ok()?;
    fs::write(dir.join("deep-sleep"), "0").ok()?;
    Some(prev)
}

// --------------------------------------------------------------------- stop

pub fn stop() -> Result<()> {
    need_root()?;
    fs::create_dir_all(paths::RUN)?;
    let pid = daemon_pid();
    if daemon_alive() {
        unsafe { libc::kill(pid, libc::SIGTERM) };
        // The daemon tears down on SIGTERM; wait for it so the caller can rely
        // on the interfaces being gone when this returns.
        if wait_until(Duration::from_secs(30), || !pid_matches(pid, "radio daemon")) {
            return Ok(());
        }
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    if let Some(st) = read_state() {
        teardown(&st, "stopped");
    }
    Ok(())
}

fn kill_and_wait(pid: i32, needle: &str) {
    if !pid_matches(pid, needle) {
        return;
    }
    unsafe { libc::kill(pid, libc::SIGTERM) };
    if !wait_until(Duration::from_secs(3), || !pid_matches(pid, needle)) {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Undo everything `start` did, in reverse. Idempotent.
fn teardown(st: &State, why: &str) {
    kill_and_wait(st.owl_pid, &owl_path());
    quiet("pkill", &["-f", &paths::lib("awdl-airdrop-adv.py").display().to_string()]);
    quiet("ip", &["link", "set", MON, "down"]);
    quiet("iw", &["dev", MON, "del"]);
    if let (Some((rpm, ds)), Some(sta)) = (&st.mt76_pm, hw::find_station()) {
        let _ = fs::write(mt76_dir(&sta.phy).join("runtime-pm"), rpm);
        let _ = fs::write(mt76_dir(&sta.phy).join("deep-sleep"), ds);
    }
    // Some drivers' stations stop receiving after a monitor interface is
    // deleted (the MT7925, a few minutes later). A fresh association right
    // after the delete has kept it healthy in every teardown tried.
    if st.reconnect {
        sleep(Duration::from_secs(1));
        if !quiet("nmcli", &["-w", "20", "device", "connect", &st.station]) {
            quiet("iw", &["dev", &st.station, "disconnect"]);
        }
    }
    let _ = fs::remove_file(paths::run("state.json"));
    let _ = fs::remove_file(paths::run("daemon.pid"));
    let _ = fs::write(paths::run("last"), why);
    log(&format!("stopped ({why})"));
}

// ------------------------------------------------------------------- daemon

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

/// The background half of a window: announce, watch OWL, end at expiry.
pub fn daemon() -> Result<()> {
    need_root()?;
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    let announce_log = OpenOptions::new().create(true).append(true).open(paths::run("announce.log"))?;
    let adv = paths::lib("awdl-airdrop-adv.py");
    let mut child: Option<std::process::Child> = None;
    let mut next = Instant::now();

    let why = loop {
        if TERM.load(Ordering::SeqCst) {
            break "stopped";
        }
        let Some(st) = read_state() else { break "state-lost" };
        if st.until > 0 && now() >= st.until {
            break "expired";
        }
        if !pid_matches(st.owl_pid, &owl_path()) {
            break "owl-exited";
        }
        if let Some(c) = child.as_mut() {
            if matches!(c.try_wait(), Ok(Some(_))) {
                child = None;
            }
        }
        if child.is_none() && Instant::now() >= next {
            // --plain: OWL adds the AWDL encapsulation itself.
            child = Command::new("python3")
                .arg(&adv)
                .args(["--iface", AWDL, "--plain", "--flags", ADV_FLAGS, "--port", ADV_PORT])
                .args(["--count", "3", "--interval", "0.05"])
                .stdin(Stdio::null())
                .stdout(announce_log.try_clone()?)
                .stderr(announce_log.try_clone()?)
                .spawn()
                .ok();
            next = Instant::now() + ADV_INTERVAL;
        }
        sleep(Duration::from_millis(250));
    };
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    if let Some(st) = read_state() {
        teardown(&st, why);
    } else {
        let _ = fs::write(paths::run("last"), why);
    }
    Ok(())
}

/// Used by the doctor: what stands in the way of a window, most fundamental
/// first. `Ok` carries the channel the window would use.
pub fn preflight() -> Result<(hw::Station, Profile, u32), String> {
    let sta = hw::find_station().ok_or("no Wi-Fi interface found")?;
    let profile = hw::profile_for(&sta.driver);
    let freq = hw::link_freq(&sta.iface).ok_or("Wi-Fi isn't connected")?;
    let chan = hw::awdl_channel(freq).ok_or_else(|| {
        format!("your access point is on {freq} MHz; AirDrop needs channel 6, 44 or 149")
    })?;
    Ok((sta, profile, chan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_pad_octets_and_honour_removal() {
        let log = "12:00 add peer 86:8:4:8a:2b:f4 (iPhone)\n\
                   12:01 add peer aa:bb:cc:dd:ee:ff (Mac)\n\
                   12:02 remove peer aa:bb:cc:dd:ee:ff (Mac)\n\
                   noise\n";
        assert_eq!(parse_peers(log), vec!["86:08:04:8a:2b:f4"]);
    }

    #[test]
    fn rejects_malformed_macs() {
        assert_eq!(normalize_mac("1:2:3"), None);
        assert_eq!(normalize_mac("zz:00:00:00:00:00"), None);
        assert_eq!(normalize_mac("A:b:c:d:e:f").as_deref(), Some("0a:0b:0c:0d:0e:0f"));
    }
}
