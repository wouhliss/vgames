//! `vgames login` / `vgames logout`: Discord sign-in for the CLI (01-security §4.1).
//!
//! The desktop flow with PKCE (S256) and a random `client_state`. The browser
//! ends on the server's fallback page, which shows a one-time code (valid one
//! minute); the user pastes it, or the whole `vgames://auth/callback?…` link.
//! `--authorize-with PROGRAM` replaces the browser for scripts and tests.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};
use url::Url;
use vgames_core::sign::{PublicKey, fill_random};
use vgames_core::trust::RootPin;
use vgames_proto::auth::{
    AuthStartRequest, AuthStartResponse, ClientKind, TokenRequest, TokenResponse,
};
use vgames_proto::discovery::ServerInfo;
use zeroize::Zeroizing;

use crate::server::{Api, ServerArgs};
use crate::session::{self, Credentials, Session};

#[derive(clap::Args)]
#[command(after_help = "\
Examples:
  vgames login --server https://games.example.com
  vgames login --server https://games.example.com --fingerprint VG1-7K2M-Q9XD-…
  vgames login --server http://localhost:8080 --yes --authorize-with ./sign-in-helper

The browser opens Discord; afterwards the page shows a code. Paste it here (or the
whole vgames:// link). The code is valid for one minute.

Non-interactive: --fingerprint (or --yes) skips the fingerprint question, and
--authorize-with PROGRAM runs PROGRAM with the sign-in URL as its only argument and
reads the code (or the vgames:// link) from the first line of its output.
For a single command, VGAMES_ACCESS_TOKEN=vga_… can be used instead of a session.")]
pub struct LoginArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// Expected root key fingerprint (VG1-…), from the server owner.
    #[arg(long)]
    fingerprint: Option<String>,
    /// Trust the fingerprint shown without asking (only when you checked it another way).
    #[arg(long)]
    yes: bool,
    /// Print the sign-in URL instead of opening the browser.
    #[arg(long)]
    no_browser: bool,
    /// Get the code from PROGRAM instead of a browser and a prompt (scripts, tests).
    #[arg(long, value_name = "PROGRAM", conflicts_with = "no_browser")]
    authorize_with: Option<PathBuf>,
    /// Name of this session in the account's session list.
    #[arg(long, default_value = "vgames CLI")]
    device_name: String,
}

#[derive(clap::Args)]
#[command(after_help = "\
Examples:
  vgames logout --server https://games.example.com")]
pub struct LogoutArgs {
    #[command(flatten)]
    server: ServerArgs,
}

struct Pkce {
    verifier: Zeroizing<String>,
    challenge: String,
    client_state: String,
}

fn random_b64(n: usize) -> Result<String> {
    let mut buf = Zeroizing::new(vec![0u8; n]);
    fill_random(&mut buf).map_err(|_| anyhow::anyhow!("the OS random generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf.as_slice()))
}

fn pkce() -> Result<Pkce> {
    let verifier = Zeroizing::new(random_b64(32)?);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok(Pkce {
        verifier,
        challenge,
        client_state: random_b64(16)?,
    })
}

fn is_code(s: &str) -> bool {
    (43..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A pasted code, or a pasted `vgames://auth/callback?code=…&client_state=…` link
/// (whose `client_state` must be ours).
fn parse_code(input: &str, client_state: &str) -> Result<Zeroizing<String>> {
    let input = input.trim();
    if input.starts_with("vgames:") {
        let url = Url::parse(input).context("the pasted link is not a valid vgames:// link")?;
        if url.host_str() != Some("auth") || url.path() != "/callback" {
            bail!("the pasted link is not a sign-in link");
        }
        let mut code = None;
        let mut state = None;
        for (k, v) in url.query_pairs() {
            match &*k {
                "code" => code = Some(Zeroizing::new(v.into_owned())),
                "client_state" => state = Some(v.into_owned()),
                _ => {}
            }
        }
        if state.as_deref() != Some(client_state) {
            bail!("the pasted link belongs to another sign-in; start again");
        }
        let code = code.context("the pasted link has no code")?;
        if !is_code(&code) {
            bail!("the pasted link has a malformed code");
        }
        return Ok(code);
    }
    if !is_code(input) {
        bail!("that is not a sign-in code (43–64 letters, digits, - and _)");
    }
    Ok(Zeroizing::new(input.to_owned()))
}

fn open_browser(url: &Url) -> bool {
    if !matches!(url.scheme(), "https" | "http") {
        return false;
    }
    // One argv entry, never a shell: the URL comes from the server.
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = Command::new("xdg-open");
    cmd.arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn code_from_program(program: &PathBuf, url: &Url) -> Result<Zeroizing<String>> {
    let out = Command::new(program)
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {}", program.display()))?;
    if !out.status.success() {
        bail!("{} failed ({})", program.display(), out.status);
    }
    let text = Zeroizing::new(String::from_utf8_lossy(&out.stdout).into_owned());
    Ok(Zeroizing::new(text.lines().next().unwrap_or("").to_owned()))
}

fn prompt_line(prompt: &str) -> Result<Zeroizing<String>> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut line = Zeroizing::new(String::new());
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading the code")?;
    Ok(line)
}

/// Decides which root key to pin for this server.
fn pin_root(args: &LoginArgs, info: &ServerInfo, origin: &Url) -> Result<RootPin> {
    let root = PublicKey::from_base64(&info.root_public_key)
        .context("the server advertises an invalid root key")?;
    let fingerprint = root.fingerprint().to_string();
    if fingerprint != info.root_key_fingerprint {
        bail!("the server's root key and fingerprint do not match; refusing to continue");
    }
    // An existing pin only moves along an announced rotation.
    if let Some(existing) = session::load(origin)? {
        let pin = existing.pin()?;
        if pin.root == root {
            return Ok(pin);
        }
        if pin.next_root.as_ref() == Some(&root) {
            return Ok(RootPin::new(root));
        }
        bail!(
            "the root key of {origin} changed from {} to {fingerprint}. This can mean the server was \
             replaced or compromised. Ask the server owner; if the change is expected, run \
             `vgames logout --server {origin}` and sign in again.",
            pin.root.fingerprint()
        );
    }
    if let Some(expected) = &args.fingerprint {
        if expected.trim() != fingerprint {
            bail!(
                "the server's root key fingerprint is {fingerprint}, not {expected}; refusing to sign in"
            );
        }
        return Ok(RootPin::new(root));
    }
    eprintln!("Server:      {} ({})", info.name, info.server_id);
    eprintln!("Fingerprint: {fingerprint}");
    if args.yes {
        return Ok(RootPin::new(root));
    }
    if !std::io::stdin().is_terminal() {
        bail!("pass --fingerprint (from the server owner) to sign in non-interactively");
    }
    let answer = prompt_line(
        "Compare this fingerprint with the one the server owner gave you. Is it the same? [y/N] ",
    )?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        bail!("sign-in cancelled");
    }
    Ok(RootPin::new(root))
}

pub async fn login(args: LoginArgs) -> Result<()> {
    let origin = args.server.origin()?;
    let api = Api::new(origin.clone())?;
    let info: ServerInfo = api
        .get("/.well-known/vgames.json")
        .await
        .with_context(|| format!("{origin} does not look like a vgames server"))?;
    let pin = pin_root(&args, &info, &origin)?;

    let pkce = pkce()?;
    let device_name: String = args.device_name.chars().take(64).collect();
    let start: AuthStartResponse = api
        .post(
            "/v1/auth/discord/start",
            &AuthStartRequest {
                client: ClientKind::Desktop,
                code_challenge: Some(pkce.challenge.clone()),
                client_state: Some(pkce.client_state.clone()),
                device_name: Some(device_name),
                return_to: None,
            },
        )
        .await?;
    let authorize =
        Url::parse(&start.authorize_url).context("the server sent an invalid sign-in URL")?;
    if !matches!(authorize.scheme(), "https" | "http") {
        bail!("the server sent a sign-in URL that is not a web page");
    }

    let input = if let Some(program) = &args.authorize_with {
        code_from_program(program, &authorize)?
    } else {
        eprintln!("Sign in with Discord in your browser:\n\n  {authorize}\n");
        if !args.no_browser && !open_browser(&authorize) {
            eprintln!("(could not open a browser; open the link above yourself)");
        }
        prompt_line("Paste the code shown after signing in: ")?
    };
    let code = parse_code(&input, &pkce.client_state)?;

    let tokens: TokenResponse = api
        .post(
            "/v1/auth/token",
            &TokenRequest::AuthorizationCode {
                code: code.to_string(),
                code_verifier: pkce.verifier.to_string(),
            },
        )
        .await?;
    let creds = Credentials::new(&origin, info.server_id, &pin, &tokens)?;
    session::save_and_report(&creds)?;
    println!(
        "Signed in to {} as {} ({}).",
        info.name,
        tokens.user.username,
        match tokens.user.role {
            vgames_proto::auth::Role::User => "user",
            vgames_proto::auth::Role::Admin => "admin",
            vgames_proto::auth::Role::Owner => "owner",
        }
    );
    Ok(())
}

pub async fn logout(args: LogoutArgs) -> Result<()> {
    let origin = args.server.origin()?;
    if session::load(&origin)?.is_none() && std::env::var_os("VGAMES_ACCESS_TOKEN").is_none() {
        println!("Not signed in to {origin}.");
        return Ok(());
    }
    let revoked = match Session::open(&args.server).await {
        Ok(s) => s.api.post_empty("/v1/auth/logout").await,
        Err(e) => Err(e),
    };
    session::delete(&origin)?;
    match revoked {
        Ok(()) => println!("Signed out of {origin}."),
        Err(e) => println!("Removed the local session for {origin} (the server said: {e:#})."),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636() {
        // RFC 7636 appendix B.
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(
            b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let p = pkce().unwrap();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(p.challenge.len(), 43);
        assert!(p.client_state.len() >= 16);
    }

    #[test]
    fn pasted_codes_and_links() {
        let code = "A".repeat(43);
        assert_eq!(*parse_code(&format!("  {code}\n"), "cs").unwrap(), code);
        let link = format!("vgames://auth/callback?code={code}&client_state=abcdefghijklmnop");
        assert_eq!(*parse_code(&link, "abcdefghijklmnop").unwrap(), code);
        // Another sign-in's link, a bad code, another route.
        assert!(parse_code(&link, "qrstuvwxyzabcdef").is_err());
        assert!(parse_code("short", "cs").is_err());
        assert!(parse_code(&format!("{code};rm -rf /"), "cs").is_err());
        assert!(
            parse_code(
                &format!("vgames://launch/x?code={code}&client_state=cs"),
                "cs"
            )
            .is_err()
        );
    }
}
