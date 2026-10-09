mod doctor;
mod hw;
mod paths;
mod privilege;
mod radio;
mod recv;
mod send;
mod session;
mod setup;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// AirDrop for Linux. Send and receive files with nearby iPhones and Macs.
#[derive(Parser)]
#[command(name = "yeetdrop", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install everything: packages, OWL, the sender and receiver
    Setup {
        /// Run on a Wi-Fi driver that hasn't been tested yet
        #[arg(long)]
        allow_untested: bool,
    },
    /// Check what works and what doesn't
    Doctor,
    /// Become discoverable and receive until you press Ctrl-C
    On {
        /// How long to stay discoverable: 10m, 90s, 2h, forever
        #[arg(short = 't', long = "for", default_value = "10m")]
        duration: String,
        /// The name other devices show for this machine
        #[arg(long)]
        name: Option<String>,
    },
    /// Close the window from another terminal
    Off,
    /// Show whether the window is open
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List devices heard in the current window
    Peers,
    /// Send files to a nearby device
    Send {
        /// Device address from `yeetdrop peers` (needed if several are nearby)
        #[arg(long)]
        to: Option<String>,
        #[arg(required = true)]
        files: Vec<String>,
    },
    #[command(hide = true, subcommand)]
    Radio(RadioCmd),
    #[command(hide = true, name = "__install")]
    Install {
        #[arg(long)]
        stage: PathBuf,
    },
    #[command(hide = true, name = "__allow-untested")]
    AllowUntested,
}

#[derive(Subcommand)]
enum RadioCmd {
    Start {
        #[arg(long)]
        secs: u64,
    },
    Stop,
    Daemon,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Setup { allow_untested } => setup::run(allow_untested),
        Cmd::Doctor => {
            let r = doctor::run(false);
            println!();
            if r.failed > 0 {
                println!("{} problem(s), {} warning(s)", r.failed, r.warned);
                return ExitCode::from(1);
            }
            println!("{}", if r.warned > 0 { format!("works, {} warning(s)", r.warned) } else { "all good".into() });
            Ok(())
        }
        Cmd::On { duration, name } => session::on(&duration, name.as_deref()),
        Cmd::Off => session::off(),
        Cmd::Status { json } => session::status(json),
        Cmd::Peers => session::peers(),
        Cmd::Send { to, files } => send::send(&files, to.as_deref()),
        Cmd::Radio(RadioCmd::Start { secs }) => radio::start(secs).map(|s| {
            if matches!(s, radio::Started::Extended) {
                // Helper contract: 5 means the window already existed.
                std::process::exit(5);
            }
        }),
        Cmd::Radio(RadioCmd::Stop) => radio::stop(),
        Cmd::Radio(RadioCmd::Daemon) => radio::daemon(),
        Cmd::Install { stage } => setup::install_staged(&stage),
        Cmd::AllowUntested => setup::allow_untested_flag(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Radio errors carry the helper-contract exit code for the caller.
            if let Some(r) = e.downcast_ref::<radio::RadioError>() {
                eprintln!("{r}");
                return ExitCode::from(r.code as u8);
            }
            eprintln!("{} {e:#}", ui::fail_mark());
            ExitCode::from(1)
        }
    }
}
