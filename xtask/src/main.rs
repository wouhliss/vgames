//! `cargo xtask` — repository automation. Owner: Agent 5.
//!
//! Planned commands:
//! - `changelog lint`             validate `.changes/*.md` fragments (CI, every PR)
//! - `changelog release <ver>`    fold fragments into CHANGELOG.md + user-facing JSON
//! - `openapi check`              fail if the API's generated spec drifts from openapi/openapi.yaml

fn main() {
    eprintln!("xtask: not implemented yet");
    std::process::exit(2);
}
