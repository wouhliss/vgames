//! `vgames`: key ceremonies, trust bundles and publishing (A5-T06).
//!
//! - `keys init-root | issue-publisher | show`   offline key ceremonies
//! - `trust build | sign | verify`               root-signed trust bundles
//! - `login`, `logout`                           Discord sign-in, tokens in the OS keychain
//! - `trust publish | re-sign`                   bundles and signatures on the server
//!
//! `publish` (packing and uploading a version) lands with Agent 2's upload library.

mod keys;
mod login;
mod secret;
mod server;
mod session;
mod trust;
mod trust_server;

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
Then, online:
  vgames login --server https://games.example.com --fingerprint VG1-…
  vgames trust publish --server https://games.example.com --signed bundle.signed.json

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
    /// Sign in to a server with Discord (tokens go to the OS keychain).
    Login(login::LoginArgs),
    /// Sign out of a server and forget its tokens.
    Logout(login::LogoutArgs),
}

/// Runs a server command on a small runtime (the offline commands need none).
pub(crate) fn block_on<F: std::future::Future<Output = anyhow::Result<()>>>(
    f: F,
) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(f)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Keys { command } => keys::run(command),
        Cmd::Trust { command } => trust::run(command),
        Cmd::Login(args) => block_on(login::login(args)),
        Cmd::Logout(args) => block_on(login::logout(args)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
