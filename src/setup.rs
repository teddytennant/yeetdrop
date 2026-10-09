//! `setup`: dependencies, OWL at a pinned commit plus our patches, the AirDrop
//! sender and receiver, and a root-owned install, with progress you can read.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use console::style;

use crate::ui::{run_logged, Step};
use crate::{doctor, hw, paths, privilege};

// Pinned upstream commits. Bump deliberately: the patches apply to OWL_COMMIT.
const OWL_REPO: &str = "https://github.com/jedbillyb/owl.git";
const OWL_COMMIT: &str = "832d70f815c3d4a06a02117bf0fc5e868daa1ff0";
const AWDL_REPO: &str = "https://github.com/brentkearney/omdrop-awdl.git";
const AWDL_COMMIT: &str = "534f91a525337951ca19c311e53d333bd6d55100";
const PLUGIN_REPO: &str = "https://github.com/brentkearney/omdrop-plugin.git";
const PLUGIN_COMMIT: &str = "58fca678b08d3a7c875c83c7dfc2f0d3251cce44";

const PATCHES: [(&str, &str); 2] = [
    ("owl-01-tx-rate.patch", include_str!("../patches/owl-01-tx-rate.patch")),
    ("owl-02-awdl-addr.patch", include_str!("../patches/owl-02-awdl-addr.patch")),
];

const SHIM: &str = include_str!("../packaging/omdrop-discoverable");
const POLICY: &str = include_str!("../packaging/io.yeetdrop.peers.policy");
const NM_CONF: &str = include_str!("../packaging/99-yeetdrop-unmanaged.conf");

/// Radio-independent tools from omdrop-awdl, installed unmodified.
const FROM_AWDL: [&str; 8] = [
    "send-to-peer", "airdrop-send.py", "awdl-af-parse.py", "awdl_identity.py",
    "awdl_debug.py", "ble-airdrop-adv.py", "awdl-airdrop-adv.py", "awdl-mdns-respond.py",
];
const FROM_PLUGIN: [&str; 4] = ["airdrop-serve.py", "contacts.py", "debugdump.py", "identity.py"];

#[derive(Clone, Copy, PartialEq)]
enum Pm { Apt, Pacman, Dnf }

impl Pm {
    fn detect() -> Option<Pm> {
        let os = fs::read_to_string("/etc/os-release").unwrap_or_default().to_ascii_lowercase();
        let has = |w: &str| os.lines().any(|l| (l.starts_with("id=") || l.starts_with("id_like=")) && l.contains(w));
        if has("debian") || has("ubuntu") { Some(Pm::Apt) }
        else if has("arch") { Some(Pm::Pacman) }
        else if has("fedora") || has("rhel") { Some(Pm::Dnf) }
        else { None }
    }

    fn packages(self) -> &'static [&'static str] {
        match self {
            Pm::Apt => &["git", "build-essential", "cmake", "libpcap-dev", "libev-dev", "libnl-3-dev",
                "libnl-genl-3-dev", "iw", "iproute2", "iputils-ping", "network-manager", "polkitd", "pkexec",
                "keyutils", "bluez", "python3", "python3-libarchive-c", "python3-pil", "python3-zeroconf",
                "python3-ifaddr", "python3-dbus", "python3-gi"],
            Pm::Pacman => &["git", "base-devel", "cmake", "libpcap", "libev", "libnl", "iw", "iproute2",
                "iputils", "networkmanager", "polkit", "keyutils", "bluez", "python", "python-libarchive-c",
                "python-pillow", "python-zeroconf", "python-ifaddr", "python-dbus", "python-gobject"],
            Pm::Dnf => &["git", "gcc", "make", "cmake", "libpcap-devel", "libev-devel", "libnl3-devel", "iw",
                "iproute", "iputils", "NetworkManager", "polkit", "keyutils", "bluez", "python3",
                "python3-libarchive-c", "python3-pillow", "python3-zeroconf", "python3-ifaddr",
                "python3-dbus", "python3-gobject"],
        }
    }

    fn installed(self, pkg: &str) -> bool {
        let (cmd, args): (&str, Vec<&str>) = match self {
            Pm::Apt => ("dpkg-query", vec!["-W", "-f=${Status}", pkg]),
            Pm::Pacman => ("pacman", vec!["-Qi", pkg]),
            Pm::Dnf => ("rpm", vec!["-q", pkg]),
        };
        match Command::new(cmd).args(args).output() {
            Ok(o) if o.status.success() => self != Pm::Apt || String::from_utf8_lossy(&o.stdout).ends_with("installed"),
            _ => false,
        }
    }

    fn install_cmd(self, pkgs: &[&str]) -> Command {
        let mut c = Command::new("sudo");
        match self {
            Pm::Apt => c.args(["apt-get", "install", "-y"]),
            Pm::Pacman => c.args(["pacman", "-S", "--needed", "--noconfirm"]),
            Pm::Dnf => c.args(["dnf", "install", "-y"]),
        };
        c.args(pkgs);
        c
    }
}

pub fn run(allow_untested: bool) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("yeetdrop only runs on Linux");
    }
    println!("{}\n", style("yeetdrop setup").bold());
    let work = paths::cache_dir();
    let log = work.join("setup.log");
    let _ = fs::remove_file(&log);
    fs::create_dir_all(&work)?;

    // 1. Hardware
    let step = Step::new("Checking your Wi-Fi card");
    let Some(sta) = hw::find_station() else {
        step.fail("No Wi-Fi interface found");
        bail!("AirDrop needs a Wi-Fi card that can inject frames in monitor mode");
    };
    let profile = hw::profile_for(&sta.driver);
    if profile.tested {
        step.ok(format!("{} ({}): tested", sta.iface, sta.driver));
    } else if allow_untested {
        step.warn(format!("{} ({}): untested, continuing because you asked", sta.iface, sta.driver));
    } else {
        step.warn(format!("{} ({}): untested", sta.iface, sta.driver));
        println!(
            "  {}\n  Continue anyway with: {}",
            style("This driver hasn't been verified to inject while connected.").dim(),
            style("yeetdrop setup --allow-untested").bold()
        );
        bail!("stopped before changing anything");
    }

    privilege::prime()?;

    // 2. Dependencies
    let Some(pm) = Pm::detect() else {
        bail!("unsupported distribution: install the build and runtime dependencies from the README by hand");
    };
    let step = Step::new("Checking packages");
    let missing: Vec<&str> = pm.packages().iter().copied().filter(|p| !pm.installed(p)).collect();
    if missing.is_empty() {
        step.ok("All packages present");
    } else {
        step.set(format!("Installing {} packages", missing.len()));
        match run_logged(&mut pm.install_cmd(&missing), &log) {
            Ok(()) => step.ok(format!("Installed {}", missing.join(", "))),
            Err(e) => {
                step.fail("Package install failed");
                return Err(e);
            }
        }
    }

    // 3. Sources
    let src = work.join("src");
    let owl_dir = src.join("owl");
    let step = Step::new("Fetching sources at pinned commits");
    for (name, repo, commit) in [("owl", OWL_REPO, OWL_COMMIT), ("omdrop-awdl", AWDL_REPO, AWDL_COMMIT), ("omdrop-plugin", PLUGIN_REPO, PLUGIN_COMMIT)] {
        step.set(format!("Fetching {name}"));
        checkout(&src.join(name), repo, commit, &log).inspect_err(|_| step.println(format!("{} {name}", crate::ui::fail_mark())))?;
    }
    step.set("Fetching OWL's submodules");
    run_logged(Command::new("git").current_dir(&owl_dir).args(["submodule", "update", "--init", "--recursive"]), &log)?;
    step.ok(format!("owl@{} omdrop-awdl@{} omdrop-plugin@{}", &OWL_COMMIT[..7], &AWDL_COMMIT[..7], &PLUGIN_COMMIT[..7]));

    // 4. Patch and build
    let step = Step::bar("Building OWL");
    let patch_dir = work.join("patches");
    fs::create_dir_all(&patch_dir)?;
    for (name, body) in PATCHES {
        let p = patch_dir.join(name);
        fs::write(&p, body)?;
        run_logged(Command::new("git").current_dir(&owl_dir).arg("apply").arg(&p), &log).context("applying patches to OWL")?;
    }
    let build = owl_dir.join("build");
    // The policy minimum is for the bundled googletest, whose cmake_minimum_required
    // predates 3.5, which CMake 4 refuses. Only the owl target is built.
    run_logged(
        Command::new("cmake").args(["-S"]).arg(&owl_dir).arg("-B").arg(&build)
            .args(["-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_POLICY_VERSION_MINIMUM=3.5", "-Wno-dev"]),
        &log,
    )?;
    if let Err(e) = build_owl(&owl_dir, &build, &log, &step) {
        step.fail("OWL didn't build");
        return Err(e);
    }
    let owl_bin = build.join("daemon/owl");
    let probe = Command::new(&owl_bin).args(["-S", "intersect", "-R", "12", "-m", "02:00:00:00:00:01", "-h"]).output()?;
    let text = String::from_utf8_lossy(&[probe.stdout, probe.stderr].concat()).into_owned();
    if text.contains("invalid option") {
        step.fail("OWL built without our patches' flags");
        bail!("owl lacks -S/-R/-m; the patches did not take");
    }
    step.ok("OWL built and patched");

    // 5. Stage, then install as root
    let step = Step::new("Installing to /usr/lib/yeetdrop");
    let stage = work.join("stage");
    let _ = fs::remove_dir_all(&stage);
    stage_tree(&stage, &src, &owl_bin)?;
    let out = privilege::command(&["__install", "--stage", &stage.display().to_string()])?.output()?;
    if !out.status.success() {
        step.fail("Install failed");
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    step.ok("Installed (root-owned)");

    if allow_untested {
        privilege::command(&["__allow-untested"])?.status()?;
    }

    // 6. Verify
    println!();
    let report = doctor::run(false);
    println!();
    if report.failed == 0 {
        println!("{}  Try: {}", style("Done.").green().bold(), style("yeetdrop on").bold());
    } else {
        println!("{}", style("Installed, but some checks need attention (above).").yellow());
    }
    Ok(())
}

/// Pinned shallow checkout; reusable across runs.
fn checkout(dir: &Path, repo: &str, commit: &str, log: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let git = |args: &[&str]| run_logged(Command::new("git").current_dir(dir).args(args), log);
    if !dir.join(".git").exists() {
        git(&["init", "-q"])?;
        git(&["remote", "add", "origin", repo])?;
    }
    git(&["fetch", "-q", "--depth", "1", "origin", commit])?;
    git(&["-c", "advice.detachedHead=false", "checkout", "-q", "-f", "FETCH_HEAD"])?;
    // Drop earlier patches so they apply to a clean tree, keeping the build dir.
    git(&["clean", "-fdxq", "-e", "build"])?;
    Ok(())
}

/// Run the build, turning cmake's `[ 42%]` lines into the progress bar.
fn build_owl(owl_dir: &Path, build: &Path, log: &Path, step: &Step) -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mut logf = fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut child = Command::new("cmake")
        .current_dir(owl_dir)
        .arg("--build").arg(build)
        .args(["--target", "owl", "-j"])
        .arg(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(logf.try_clone()?))
        .spawn()?;
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        let _ = writeln!(logf, "{line}");
        if let Some(pct) = line.trim_start().strip_prefix('[').and_then(|r| r.split('%').next()).and_then(|n| n.trim().parse().ok()) {
            step.progress(pct);
        }
    }
    if !child.wait()?.success() {
        bail!("cmake --build failed\n{}\nfull log: {}", crate::ui::tail(log, 12), log.display());
    }
    Ok(())
}

fn stage_tree(stage: &Path, src: &Path, owl_bin: &Path) -> Result<()> {
    let put = |from: PathBuf, to: &str, exec: bool| -> Result<()> {
        let dest = stage.join(to);
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::copy(&from, &dest).with_context(|| format!("staging {}", from.display()))?;
        set_mode(&dest, if exec { 0o755 } else { 0o644 })
    };
    put(owl_bin.to_path_buf(), "owl", true)?;
    for f in FROM_AWDL {
        put(src.join("omdrop-awdl/userspace").join(f), f, !f.ends_with(".py") || f != "awdl_identity.py" && f != "awdl_debug.py" && f != "awdl-af-parse.py")?;
    }
    for f in FROM_PLUGIN {
        put(src.join("omdrop-plugin/bin").join(f), &format!("receiver/bin/{f}"), f == "airdrop-serve.py")?;
    }
    for entry in fs::read_dir(src.join("omdrop-plugin/bin/apple-ca"))? {
        let p = entry?.path();
        if p.is_file() {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            put(p, &format!("receiver/bin/apple-ca/{name}"), false)?;
        }
    }
    put(src.join("omdrop-plugin/share/certs/apple_root_ca.pem"), "receiver/share/certs/apple_root_ca.pem", false)?;
    for (from, to) in [
        ("owl/COPYING", "licenses/OWL-GPL-3.0"),
        ("owl/radiotap/COPYING", "licenses/radiotap-ISC"),
        ("omdrop-awdl/LICENSE", "licenses/omdrop-awdl-LICENSE"),
        ("omdrop-plugin/LICENSE", "licenses/omdrop-plugin-LICENSE"),
    ] {
        put(src.join(from), to, false)?;
    }
    fs::write(stage.join("omdrop-discoverable"), SHIM)?;
    set_mode(&stage.join("omdrop-discoverable"), 0o755)?;
    fs::write(stage.join("VERSION"), format!("{}\n", env!("CARGO_PKG_VERSION")))?;
    fs::write(stage.join("io.yeetdrop.peers.policy"), POLICY)?;
    fs::write(stage.join("99-yeetdrop-unmanaged.conf"), NM_CONF)?;
    Ok(())
}

fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))?;
    Ok(())
}

// ---------------------------------------------------------------- root half

/// Copy a staged tree into place. Runs as root, copying only regular files and
/// directories (never following symlinks), and swaps the tree in at the end so
/// an interrupted install leaves the previous one intact.
pub fn install_staged(stage: &Path) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("install needs root");
    }
    let new = PathBuf::from(format!("{}.new", paths::LIB));
    let old = PathBuf::from(format!("{}.old", paths::LIB));
    let _ = fs::remove_dir_all(&new);
    let _ = fs::remove_dir_all(&old);
    copy_tree(stage, &new, true)?;
    // These two live elsewhere on the system; move them out of the tree.
    for (name, dest) in [("io.yeetdrop.peers.policy", paths::POLKIT_POLICY), ("99-yeetdrop-unmanaged.conf", paths::NM_CONF)] {
        let from = new.join(name);
        if let Some(dir) = Path::new(dest).parent() {
            fs::create_dir_all(dir)?;
        }
        fs::copy(&from, dest)?;
        set_mode(Path::new(dest), 0o644)?;
        fs::remove_file(from)?;
    }
    if Path::new(paths::LIB).exists() {
        fs::rename(paths::LIB, &old)?;
    }
    fs::rename(&new, paths::LIB)?;
    let _ = fs::remove_dir_all(&old);
    // NetworkManager reads conf.d at startup or on a reload; this touches no connection.
    let _ = Command::new("nmcli").args(["general", "reload", "conf"]).output();
    Ok(())
}

fn copy_tree(from: &Path, to: &Path, top: bool) -> Result<()> {
    fs::create_dir_all(to)?;
    set_mode(to, 0o755)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest, false)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &dest)?;
            use std::os::unix::fs::PermissionsExt;
            let exec = fs::metadata(entry.path())?.permissions().mode() & 0o111 != 0;
            set_mode(&dest, if exec { 0o755 } else { 0o644 })?;
        } else {
            bail!("refusing to install {}: not a regular file", entry.path().display());
        }
    }
    let _ = top;
    Ok(())
}

pub fn allow_untested_flag() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("needs root");
    }
    fs::create_dir_all(paths::CONF)?;
    fs::write(Path::new(paths::CONF).join("allow-untested"), "1\n")?;
    Ok(())
}
