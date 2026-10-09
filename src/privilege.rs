//! Running the radio as root. The root-side code is this same binary, run
//! through sudo, so there is one implementation and nothing to keep in step.

use std::process::{Command, Stdio};

use anyhow::{bail, Result};

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Ask for the password up front, on the terminal, so it never lands in the
/// middle of a spinner.
pub fn prime() -> Result<()> {
    if is_root() {
        return Ok(());
    }
    // Passwordless sudo (or a warm timestamp) needs no prompt; `sudo -v` alone
    // insists on a terminal even then.
    let quiet = Command::new("sudo").args(["-n", "true"]).stderr(Stdio::null()).status();
    if quiet.map(|s| s.success()).unwrap_or(false) {
        return Ok(());
    }
    let ok = Command::new("sudo").arg("-v").status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        bail!("couldn't get administrator rights (sudo -v failed)");
    }
    Ok(())
}

/// `yeetdrop <args>` as root.
pub fn command(args: &[&str]) -> Result<Command> {
    let exe = std::env::current_exe()?;
    let mut c = if is_root() {
        Command::new(exe)
    } else {
        let mut c = Command::new("sudo");
        c.arg(exe);
        c
    };
    c.args(args);
    Ok(c)
}

pub struct Outcome {
    pub code: i32,
    pub stderr: String,
}

/// Run a privileged subcommand to completion, keeping its stderr for the
/// caller to present.
pub fn run(args: &[&str]) -> Result<Outcome> {
    let out = command(args)?.stdin(Stdio::null()).output()?;
    Ok(Outcome {
        code: out.status.code().unwrap_or(1),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}
