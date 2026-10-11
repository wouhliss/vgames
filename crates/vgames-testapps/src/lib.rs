//! Windows test games (GAME-05, GAME-08, GAME-09): each draws one known colour for
//! N frames in its own window, reads the last frame back, checks the colour and
//! exits 0. A non-zero exit code names the step that failed ([`Step`]), so a CI
//! log shows where Proton, Wine or the overlay broke a game.
//!
//! `vgames-testapp-<api> [--frames N] [--color R,G,B]` (defaults: 120 frames,
//! `32,160,96`). Built for `x86_64-pc-windows-gnu` (and natively on Windows); on
//! other hosts the binaries only print that they need Windows.

use std::process::ExitCode;

/// The step that failed, as the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Step {
    Arguments = 2,
    Window = 10,
    Device = 11,
    SwapChain = 12,
    Render = 13,
    Readback = 14,
    WrongColor = 15,
}

impl Step {
    pub fn fail(self, detail: impl std::fmt::Display) -> ExitCode {
        eprintln!("vgames test app: {self:?} failed: {detail}");
        ExitCode::from(self as u8)
    }
}

/// What to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub frames: u32,
    pub color: [u8; 3],
}

impl Default for Options {
    fn default() -> Self {
        Self {
            frames: 120,
            color: [32, 160, 96],
        }
    }
}

impl Options {
    /// Parses `--frames N` and `--color R,G,B`.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
            match arg.as_str() {
                "--frames" => {
                    options.frames = value
                        .parse()
                        .ok()
                        .filter(|n| (1..=100_000).contains(n))
                        .ok_or("--frames must be 1 to 100000")?;
                }
                "--color" => {
                    let parts: Vec<u8> = value
                        .split(',')
                        .map(|p| p.trim().parse::<u8>())
                        .collect::<Result<_, _>>()
                        .map_err(|_| "--color must be R,G,B with 0-255 each")?;
                    options.color = parts
                        .try_into()
                        .map_err(|_| "--color must have three components")?;
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(options)
    }

    /// The colour as normalized floats with alpha 1.
    pub fn rgba(&self) -> [f32; 4] {
        let [r, g, b] = self.color;
        [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ]
    }

    /// Whether a read-back BGRA or RGBA pixel shows the colour (±2 per channel).
    pub fn matches(&self, pixel: [u8; 4], bgra: bool) -> bool {
        let [r, g, b] = self.color;
        let got = if bgra {
            [pixel[2], pixel[1], pixel[0]]
        } else {
            [pixel[0], pixel[1], pixel[2]]
        };
        got.iter()
            .zip([r, g, b])
            .all(|(have, want)| have.abs_diff(want) <= 2)
    }
}

pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 480;

#[cfg(windows)]
pub mod win;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse() {
        let o = Options::parse(["--frames", "5", "--color", "1,2,3"].map(String::from)).unwrap();
        assert_eq!(
            o,
            Options {
                frames: 5,
                color: [1, 2, 3]
            }
        );
        assert_eq!(Options::parse(Vec::new()).unwrap(), Options::default());
        assert!(Options::parse(["--frames", "0"].map(String::from)).is_err());
        assert!(Options::parse(["--color", "1,2"].map(String::from)).is_err());
        assert!(Options::parse(["--color", "1,2,300"].map(String::from)).is_err());
        assert!(Options::parse(["--frames"].map(String::from)).is_err());
        assert!(Options::parse(["--other", "1"].map(String::from)).is_err());
    }

    #[test]
    fn pixels_match_in_either_order() {
        let o = Options {
            frames: 1,
            color: [32, 160, 96],
        };
        assert!(o.matches([96, 160, 32, 255], true));
        assert!(o.matches([33, 159, 96, 255], false));
        assert!(!o.matches([32, 160, 96, 255], true));
        assert_eq!(Step::WrongColor as u8, 15);
    }
}
