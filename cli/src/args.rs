use std::{ffi::OsString, fmt, str::FromStr};

use clap::{ArgGroup, Parser};

const MIN_DISK_SIZE: u64 = 16 * 1024 * 1024;
const MAX_DISK_SIZE: u64 = 256 * 1024 * 1024;
const LOGICAL_SECTOR_SIZE: u64 = 512;

#[derive(Debug, Parser)]
#[command(
    name = "wispdisk",
    version,
    about = "Create and remove volatile virtual disks on Windows",
    override_usage = "wispdisk.exe /add (/hdd|/rem) /letter <LETTER> /size <SIZE>\n       wispdisk.exe /del /letter <LETTER>",
    group(ArgGroup::new("operation").required(true).multiple(false).args(["add", "delete"])),
    group(ArgGroup::new("media").multiple(false).args(["hdd", "removable"]))
)]
pub struct Cli {
    /// Create a virtual disk.
    #[arg(long)]
    add: bool,

    /// Remove a virtual disk.
    #[arg(long = "del")]
    delete: bool,

    /// Present the new disk as fixed media.
    #[arg(long)]
    hdd: bool,

    /// Present the new disk as removable media.
    #[arg(long = "rem")]
    removable: bool,

    /// Requested DOS drive letter, with or without a colon.
    #[arg(long, value_name = "LETTER")]
    letter: Option<DriveLetter>,

    /// Disk size, for example 64MiB, 128MiB, or 256MiB.
    #[arg(long, value_name = "SIZE")]
    size: Option<ByteSize>,

    /// Validate and print the request without touching the driver.
    #[arg(long, hide = true)]
    pub dry_run: bool,
}

impl Cli {
    pub fn into_command(self) -> Result<Command, ValidationError> {
        let letter = self.letter.ok_or(ValidationError::MissingLetter)?;

        if self.add {
            let media = match (self.hdd, self.removable) {
                (true, false) => MediaKind::Fixed,
                (false, true) => MediaKind::Removable,
                _ => return Err(ValidationError::MissingMedia),
            };
            let size = self.size.ok_or(ValidationError::MissingSize)?;
            if size.0 < MIN_DISK_SIZE {
                return Err(ValidationError::SizeTooSmall);
            }
            if size.0 > MAX_DISK_SIZE {
                return Err(ValidationError::SizeTooLarge);
            }
            if size.0 % LOGICAL_SECTOR_SIZE != 0 {
                return Err(ValidationError::UnalignedSize);
            }

            Ok(Command::Add {
                letter,
                media,
                size_bytes: size.0,
            })
        } else {
            if self.hdd || self.removable {
                return Err(ValidationError::MediaOnDelete);
            }
            if self.size.is_some() {
                return Err(ValidationError::SizeOnDelete);
            }
            Ok(Command::Delete { letter })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Add {
        letter: DriveLetter,
        media: MediaKind,
        size_bytes: u64,
    },
    Delete {
        letter: DriveLetter,
    },
}

impl fmt::Display for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Add {
                letter,
                media,
                size_bytes,
            } => write!(
                formatter,
                "add {media} disk at {letter} with {size_bytes} bytes"
            ),
            Self::Delete { letter } => write!(formatter, "delete virtual disk at {letter}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind {
    Fixed,
    Removable,
}

impl fmt::Display for MediaKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed => formatter.write_str("fixed"),
            Self::Removable => formatter.write_str("removable"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriveLetter(char);

impl DriveLetter {
    #[allow(dead_code)]
    pub const fn as_ascii(self) -> u8 {
        self.0 as u8
    }
}

impl fmt::Display for DriveLetter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:", self.0)
    }
}

impl FromStr for DriveLetter {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim().trim_end_matches(':');
        let mut chars = value.chars();
        let Some(letter) = chars.next() else {
            return Err("drive letter is empty".into());
        };
        if chars.next().is_some() || !letter.is_ascii_alphabetic() {
            return Err("drive letter must be one ASCII letter, such as R or R:".into());
        }
        Ok(Self(letter.to_ascii_uppercase()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ByteSize(u64);

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let split_at = value
            .find(|character: char| !character.is_ascii_digit() && character != '_')
            .unwrap_or(value.len());
        let (number, suffix) = value.split_at(split_at);
        let number = number.replace('_', "");
        if number.is_empty() {
            return Err("size must start with an integer".into());
        }
        let amount = number
            .parse::<u64>()
            .map_err(|_| "size is not a valid 64-bit integer")?;
        let multiplier = match suffix.to_ascii_uppercase().as_str() {
            "" | "B" => 1,
            "KB" => 1_000,
            "KIB" => 1_024,
            "MB" => 1_000_000,
            "MIB" => 1_048_576,
            "GB" => 1_000_000_000,
            "GIB" => 1_073_741_824,
            "TB" => 1_000_000_000_000,
            "TIB" => 1_099_511_627_776,
            _ => {
                return Err(
                    "unknown size suffix; use B, KB, KiB, MB, MiB, GB, GiB, TB, or TiB".into(),
                );
            }
        };
        amount
            .checked_mul(multiplier)
            .map(Self)
            .ok_or_else(|| "size is too large".into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    MissingLetter,
    MissingMedia,
    MissingSize,
    MediaOnDelete,
    SizeOnDelete,
    SizeTooSmall,
    SizeTooLarge,
    UnalignedSize,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingLetter => "/letter is required",
            Self::MissingMedia => "exactly one of /hdd or /rem is required with /add",
            Self::MissingSize => "/size is required with /add",
            Self::MediaOnDelete => "/hdd and /rem are only valid with /add",
            Self::SizeOnDelete => "/size is only valid with /add",
            Self::SizeTooSmall => "disk size must be at least 16 MiB for NTFS formatting",
            Self::SizeTooLarge => "disk size must not exceed 256 MiB in this release",
            Self::UnalignedSize => "disk size must be a multiple of 512 bytes",
        };
        formatter.write_str(message)
    }
}

/// Clap deliberately uses GNU-style `--long` options. This adapter accepts the
/// requested Windows slash spelling without interpreting arbitrary `/path`
/// arguments as switches.
pub fn normalize_windows_args<I, S>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    args.into_iter()
        .map(|argument| normalize_one(argument.into()))
        .collect()
}

fn normalize_one(argument: OsString) -> OsString {
    let Some(text) = argument.to_str() else {
        return argument;
    };
    let Some(slash_option) = text.strip_prefix('/') else {
        return argument;
    };

    let (name, value) = slash_option
        .split_once(':')
        .map_or((slash_option, None), |(name, value)| (name, Some(value)));
    let canonical = match name.to_ascii_lowercase().as_str() {
        "add" => "add",
        "del" => "del",
        "hdd" => "hdd",
        "rem" => "rem",
        "letter" => "letter",
        "size" => "size",
        "dry-run" => "dry-run",
        "help" | "?" => "help",
        "version" => "version",
        _ => return argument,
    };

    match value {
        Some(value) => OsString::from(format!("--{canonical}={value}")),
        None => OsString::from(format!("--{canonical}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Cli {
        Cli::try_parse_from(normalize_windows_args(arguments.iter().copied())).unwrap()
    }

    #[test]
    fn accepts_requested_slash_syntax() {
        let parsed = parse(&["wispdisk.exe", "/add", "/rem", "/letter:R", "/size:128MiB"]);
        assert_eq!(
            parsed.into_command().unwrap(),
            Command::Add {
                letter: DriveLetter('R'),
                media: MediaKind::Removable,
                size_bytes: 128 * 1024 * 1024,
            }
        );
    }

    #[test]
    fn accepts_delete_command() {
        let parsed = parse(&["wispdisk.exe", "/del", "/letter", "s:"]);
        assert_eq!(
            parsed.into_command().unwrap(),
            Command::Delete {
                letter: DriveLetter('S')
            }
        );
    }

    #[test]
    fn rejects_both_operations() {
        let result = Cli::try_parse_from(normalize_windows_args([
            "wispdisk.exe",
            "/add",
            "/del",
            "/letter:R",
        ]));
        assert!(result.is_err());
    }

    #[test]
    fn requires_media_and_size_for_add() {
        let parsed = parse(&["wispdisk.exe", "/add", "/letter:R"]);
        assert_eq!(parsed.into_command(), Err(ValidationError::MissingMedia));

        let parsed = parse(&["wispdisk.exe", "/add", "/hdd", "/letter:R"]);
        assert_eq!(parsed.into_command(), Err(ValidationError::MissingSize));
    }

    #[test]
    fn rejects_non_sector_aligned_size() {
        let parsed = parse(&[
            "wispdisk.exe",
            "/add",
            "/hdd",
            "/letter:R",
            "/size:16777217",
        ]);
        assert_eq!(parsed.into_command(), Err(ValidationError::UnalignedSize));
    }

    #[test]
    fn rejects_size_above_driver_limit() {
        let parsed = parse(&["wispdisk.exe", "/add", "/hdd", "/letter:R", "/size:257MiB"]);
        assert_eq!(parsed.into_command(), Err(ValidationError::SizeTooLarge));
    }

    #[test]
    fn leaves_unknown_slash_arguments_untouched() {
        let normalized = normalize_windows_args(["wispdisk.exe", "/not-an-option"]);
        assert_eq!(normalized[1], OsString::from("/not-an-option"));
    }
}
