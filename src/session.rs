//! `on`, `off`, `status` and `peers`: the commands you type day to day.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use crate::recv::{self, Event, Receiver};
use crate::ui::{self, fmt_dur, Step};
use crate::{paths, privilege, radio};

/// `10m`, `90s`, `2h`, `forever`.
pub fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s == "forever" || s == "0" {
        return Ok(0);
    }
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => (s.as_str(), 60),
    };
    match num.parse::<u64>() {
        Ok(n) if n > 0 => Ok(n * mult),
        _ => bail!("can't read {s:?} as a duration; try 10m, 90s, 2h or forever"),
    }
}

/// Bring the radio up (or extend a running window). Returns the channel.
pub fn radio_up(secs: u64) -> Result<u32> {
    let step = Step::new("Starting the radio");
    let out = privilege::run(&["radio", "start", "--secs", &secs.to_string()])?;
    match out.code {
        0 | 5 => {
            let ch = radio::status().channel.unwrap_or(0);
            step.ok(if out.code == 5 {
                format!("Window extended (channel {ch})")
            } else {
                format!("Radio up on channel {ch}")
            });
            Ok(ch)
        }
        code => {
            step.fail(match code {
                3 => "Can't open a window right now",
                4 => "No usable AWDL radio",
                _ => "Radio failed to start",
            });
            bail!("{}", if out.stderr.is_empty() { format!("exit code {code}") } else { out.stderr });
        }
    }
}

pub fn radio_down() -> Result<()> {
    let step = Step::new("Stopping the radio");
    let out = privilege::run(&["radio", "stop"])?;
    if out.code == 0 {
        step.ok("Radio off, Wi-Fi restored");
        Ok(())
    } else {
        step.fail("Couldn't stop cleanly");
        bail!("{}", out.stderr);
    }
}

pub fn on(duration: &str, name: Option<&str>) -> Result<()> {
    let secs = parse_duration(duration)?;
    privilege::prime()?;

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))?;
    }

    radio_up(secs)?;

    let step = Step::new("Starting the receiver");
    let mut rx = Receiver::start(name)?;
    let shown_name = match rx.wait_ready(Duration::from_secs(15)) {
        Ok(n) => {
            step.ok(format!("Receiving as {}", style(&n).bold()));
            n
        }
        Err(why) => {
            step.fail("Receiver didn't start");
            rx.stop();
            let _ = radio_down();
            bail!("{why}\n  log: {}", paths::state_dir().join("receiver.log").display());
        }
    };

    println!("{}", style("Open the share sheet on your iPhone/Mac and pick this machine.").dim());
    let pb = if ui::tty() { ProgressBar::new_spinner() } else { ProgressBar::hidden() };
    pb.set_style(ProgressStyle::with_template("{spinner:.green} {msg}").unwrap());
    pb.enable_steady_tick(Duration::from_millis(100));

    let mut st = radio::status();
    let mut next_poll = Instant::now();
    let mut gone = 0;
    let end_reason = loop {
        if stop.load(Ordering::SeqCst) {
            break "Interrupted";
        }
        while let Ok(ev) = rx.events.try_recv() {
            match ev {
                Event::Incoming(s) => pb.println(format!("{} incoming: {s}", style("↓").cyan().bold())),
                Event::Saved(s) => pb.println(format!("{} saved {s}", ui::ok_mark())),
                Event::Problem(s) => pb.println(format!("{} {s}", ui::warn_mark())),
                Event::Ready { .. } => {}
            }
        }
        if !rx.alive() {
            break "The receiver stopped";
        }
        if Instant::now() >= next_poll {
            st = radio::status();
            next_poll = Instant::now() + Duration::from_secs(1);
            gone = if st.visible { 0 } else { gone + 1 };
            if gone >= 3 {
                break "The window closed";
            }
        }
        let left = match st.remaining {
            Some(s) => format!("{} left", fmt_dur(s)),
            None => "until stopped".into(),
        };
        let nearby = match st.peers {
            0 => "no devices heard yet".to_string(),
            1 => "1 device nearby".to_string(),
            n => format!("{n} devices nearby"),
        };
        pb.set_message(format!(
            "Discoverable as {} · ch {} · {left} · {nearby} · {}",
            style(&shown_name).bold(),
            st.channel.unwrap_or(0),
            style("Ctrl-C to stop").dim()
        ));
        std::thread::sleep(Duration::from_millis(200));
    };
    pb.finish_and_clear();
    println!("{end_reason}.");

    rx.stop();
    radio_down()
}

pub fn off() -> Result<()> {
    privilege::prime()?;
    recv::kill_stale();
    radio_down()
}

pub fn status(json: bool) -> Result<()> {
    let st = radio::status();
    if json {
        println!("{}", serde_json::to_string(&st)?);
        return Ok(());
    }
    if st.visible {
        let left = st.remaining.map(|s| format!(", {} left", fmt_dur(s))).unwrap_or_default();
        println!(
            "{} discoverable on channel {} via {}{left}; {} device(s) nearby",
            ui::ok_mark(),
            st.channel.unwrap_or(0),
            st.station.as_deref().unwrap_or("?"),
            st.peers
        );
    } else {
        let why = st.reason.map(|r| format!(" (last window: {r})")).unwrap_or_default();
        println!("{} not discoverable{why}", style("○").dim());
    }
    Ok(())
}

pub fn peers() -> Result<()> {
    let list = radio::peers();
    if list.is_empty() {
        println!("no devices heard. open a window with `yeetdrop on`, then open the share sheet on the other device");
    }
    for mac in list {
        println!("{mac}");
    }
    Ok(())
}
