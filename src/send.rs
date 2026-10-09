//! `send`: bring up a window if there isn't one, find a device, hand the files
//! to omdrop-awdl's sender, and close the window again if we opened it.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::session::{radio_down, radio_up};
use crate::ui::Step;
use crate::{paths, privilege, radio};

pub fn send(files: &[String], to: Option<&str>) -> Result<()> {
    for f in files {
        if !Path::new(f).is_file() {
            bail!("{f}: not a file");
        }
    }
    privilege::prime()?;
    let opened_here = !radio::status().visible;
    if opened_here {
        radio_up(180)?;
    }
    let result = run(files, to);
    if opened_here {
        radio_down()?;
    }
    result
}

fn run(files: &[String], to: Option<&str>) -> Result<()> {
    let step = Step::new("Looking for nearby devices");
    let end = Instant::now() + Duration::from_secs(25);
    while radio::peers().is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(500));
    }
    let found = radio::peers();
    if found.is_empty() {
        step.fail("Nothing nearby is listening");
        bail!("on an iPhone, open AirDrop and set it to Everyone for 10 Minutes, then try again");
    }
    step.ok(format!("{} device(s) nearby", found.len()));

    let mut cmd = Command::new("python3");
    cmd.arg(paths::lib("send-to-peer"));
    if let Some(t) = to {
        cmd.args(["--mac", t]);
    }
    cmd.args(["--wait", "30", "--name", &hostname(), "--"]).args(files);
    let status = cmd.status()?;
    if !status.success() {
        bail!("the sender exited with {status}");
    }
    Ok(())
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "Linux".into())
}
