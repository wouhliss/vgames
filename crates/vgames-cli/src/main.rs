//! `vgames`: key ceremonies, trust bundles and publishing (A5-T06).
//!
//! - `keys init-root | issue-publisher | show`   offline key ceremonies
//! - `trust build | sign | verify`               root-signed trust bundles
//!
//! Server-facing commands (`login`, `trust publish`, `publish`, `trust re-sign`)
//! land with the API endpoints and Agent 2's upload library.

mod keys;
mod secret;
mod trust;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "vgames",
    version,
    about = "vgames key ceremonies, trust bundles and publishing",
    after_help = "\
Typical server setup (owner, offline machine):
  vgames keys init-root --out root.vgkey
  vgames trust build --spec trust.toml --root root.vgkey --out bundle.json
  vgames trust sign --bundle bundle.json --root root.vgkey --out bundle.signed.json

Every command accepts --passphrase-env VAR or --passphrase-file PATH instead of the
interactive prompt, for scripts and CI."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Root and publisher key files.
    Keys {
        #[command(subcommand)]
        command: keys::KeysCmd,
    },
    /// Trust bundles: which publisher keys the server's root trusts.
    Trust {
        #[command(subcommand)]
        command: trust::TrustCmd,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Keys { command } => keys::run(command),
        Cmd::Trust { command } => trust::run(command),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
