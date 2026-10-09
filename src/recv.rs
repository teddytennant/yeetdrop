//! The receiver: omdrop's `airdrop-serve.py`, run as the invoking user (it
//! needs no privilege, and received files then belong to you).

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver as Rx, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::paths;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Ready { name: String },
    Incoming(String),
    Saved(String),
    Problem(String),
}

pub struct Receiver {
    child: Child,
    pub events: Rx<Event>,
}

/// Map one receiver log line to something worth telling the user.
pub fn parse_line(line: &str) -> Option<Event> {
    let after = |key: &str| line.find(key).map(|i| line[i + key.len()..].trim().to_string());
    if let Some(rest) = after("._airdrop._tcp.local as ") {
        let name = rest.split(" (").next().unwrap_or(&rest).to_string();
        return Some(Event::Ready { name });
    }
    if let Some(rest) = after(": ask from ") {
        return Some(Event::Incoming(rest));
    }
    if line.contains(": upload ") {
        if let Some(rest) = after(": stored ") {
            return Some(Event::Saved(rest));
        }
    }
    if line.contains(" ERROR ") || line.contains("Traceback") {
        return Some(Event::Problem(line.trim().to_string()));
    }
    None
}

impl Receiver {
    pub fn start(name: Option<&str>) -> Result<Receiver> {
        let script = paths::lib("receiver/bin/airdrop-serve.py");
        fs::create_dir_all(paths::state_dir())?;
        let mut log = File::create(paths::state_dir().join("receiver.log"))?;
        let mut cmd = Command::new("python3");
        cmd.arg(&script);
        if let Some(n) = name {
            cmd.args(["--name", n]);
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Our own group: Ctrl-C reaches us, and we stop it politely so the
            // port is freed rather than left in FIN_WAIT.
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", script.display()))?;
        fs::write(paths::state_dir().join("receiver.pid"), child.id().to_string())?;

        let (tx, rx) = mpsc::channel();
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let log2 = log.try_clone()?;
        pump(out, tx.clone(), log.try_clone()?);
        pump(err, tx, log2);
        let _ = writeln!(log, "-- receiver started");
        Ok(Receiver { child, events: rx })
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Wait for the "serving" line; `Err` carries why it never came.
    pub fn wait_ready(&mut self, limit: Duration) -> Result<String, String> {
        let end = Instant::now() + limit;
        let mut problem = None;
        loop {
            match self.events.recv_timeout(Duration::from_millis(200)) {
                Ok(Event::Ready { name }) => return Ok(name),
                Ok(Event::Problem(p)) => problem = Some(p),
                _ => {}
            }
            if !self.alive() {
                return Err(problem.unwrap_or_else(|| "the receiver exited".into()));
            }
            if Instant::now() >= end {
                return Err(problem.unwrap_or_else(|| "the receiver never started serving".into()));
            }
        }
    }

    pub fn stop(&mut self) {
        let pid = self.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_file(paths::state_dir().join("receiver.pid"));
    }
}

fn pump(src: impl std::io::Read + Send + 'static, tx: Sender<Event>, mut log: File) {
    thread::spawn(move || {
        for line in BufReader::new(src).lines().map_while(Result::ok) {
            let _ = writeln!(log, "{line}");
            if let Some(ev) = parse_line(&line) {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        }
    });
}

/// Stop a receiver left behind by a session that died (used by `off`).
pub fn kill_stale() {
    let Ok(pid) = fs::read_to_string(paths::state_dir().join("receiver.pid")) else { return };
    let Ok(pid) = pid.trim().parse::<i32>() else { return };
    let is_ours = fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).contains("airdrop-serve.py"))
        .unwrap_or(false);
    if is_ours {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let _ = fs::remove_file(paths::state_dir().join("receiver.pid"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_receiver_log() {
        let ready = "2026-10-09 12:00:00,1 INFO root: serving teddy._airdrop._tcp.local as Teddy's PC (MacBookPro18,3) on [fe80::1]:8771, receiving into /x";
        assert_eq!(parse_line(ready), Some(Event::Ready { name: "Teddy's PC".into() }));
        let ask = "t INFO root: ask from Teddy's iPhone (x): type public.jpeg, 1 item(s): IMG.jpg";
        assert!(matches!(parse_line(ask), Some(Event::Incoming(s)) if s.starts_with("Teddy's iPhone")));
        let saved = "t INFO root: upload ab: stored /home/t/Downloads/IMG.jpg";
        assert_eq!(parse_line(saved), Some(Event::Saved("/home/t/Downloads/IMG.jpg".into())));
        assert_eq!(parse_line("t DEBUG root: noise"), None);
    }
}
