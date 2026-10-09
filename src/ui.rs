//! Terminal output: spinners that degrade to plain lines when stderr isn't a
//! terminal, and the one place that decides what a check mark looks like.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use console::{style, Term};
use indicatif::{ProgressBar, ProgressStyle};

pub fn tty() -> bool {
    Term::stderr().is_term()
}

pub fn ok_mark() -> String {
    style("✔").green().bold().to_string()
}

pub fn warn_mark() -> String {
    style("!").yellow().bold().to_string()
}

pub fn fail_mark() -> String {
    style("✖").red().bold().to_string()
}

/// `9:41`, or `1:02:03` past an hour.
pub fn fmt_dur(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// One line of work: a spinner while running, a mark and a message after.
pub struct Step {
    pb: ProgressBar,
}

impl Step {
    pub fn new(msg: impl Into<String>) -> Step {
        let msg = msg.into();
        let pb = if tty() { ProgressBar::new_spinner() } else { ProgressBar::hidden() };
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg} {elapsed:.dim}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "⠿"]),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        if !tty() {
            eprintln!("… {msg}");
        }
        pb.set_message(msg);
        Step { pb }
    }

    /// A step that reports a percentage instead of spinning.
    pub fn bar(msg: impl Into<String>) -> Step {
        let s = Step::new(msg);
        if tty() {
            s.pb.set_length(100);
            s.pb.set_style(
                ProgressStyle::with_template("{spinner:.cyan} {msg:<28} [{bar:28.cyan/blue}] {pos:>3}% {elapsed:.dim}")
                    .unwrap()
                    .progress_chars("━╸─"),
            );
        }
        s
    }

    pub fn set(&self, msg: impl Into<String>) {
        self.pb.set_message(msg.into());
    }

    pub fn progress(&self, pct: u64) {
        self.pb.set_position(pct.min(100));
    }

    /// Print a line above the spinner without tearing it.
    pub fn println(&self, line: impl AsRef<str>) {
        if tty() {
            self.pb.println(line.as_ref());
        } else {
            eprintln!("{}", line.as_ref());
        }
    }

    fn finish(self, mark: String, msg: &str) {
        self.pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
        self.pb.finish_with_message(format!("{mark} {msg}"));
        if !tty() {
            eprintln!("{mark} {msg}");
        }
    }

    pub fn ok(self, msg: impl AsRef<str>) {
        self.finish(ok_mark(), msg.as_ref());
    }

    pub fn warn(self, msg: impl AsRef<str>) {
        self.finish(warn_mark(), msg.as_ref());
    }

    pub fn fail(self, msg: impl AsRef<str>) {
        self.finish(fail_mark(), msg.as_ref());
    }
}

/// Run a command with its output going to `log`. On failure the error carries
/// the tail of the log, so a failed build says why instead of just "exit 2".
pub fn run_logged(cmd: &mut Command, log: &Path) -> Result<()> {
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let f = fs::OpenOptions::new().create(true).append(true).open(log)?;
    let status = cmd
        .stdin(Stdio::null())
        .stdout(f.try_clone()?)
        .stderr(f)
        .status()
        .with_context(|| format!("running {:?}", cmd.get_program()))?;
    if !status.success() {
        bail!("{:?} failed ({status})\n{}\nfull log: {}", cmd.get_program(), tail(log, 12), log.display());
    }
    Ok(())
}

pub fn tail(path: &Path, n: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
}
