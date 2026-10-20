use std::{ffi::OsString, fmt, str::FromStr};

use clap::{ArgGroup, Parser};

const MIN_DISK_SIZE: u64 = 16 * 1024 * 1024;
const MIN_FAT32_DISK_SIZE: u64 = 64 * 1024 * 1024;
const MAX_DISK_SIZE: u64 = 256 * 1024 * 1024;
const LOGICAL_SECTOR_SIZE: u64 = 512;

#[derive(Debug, Parser)]
#[command(
    name = "wispdisk",
    version,
    about = "Create and remove volatile virtual disks on Windows",
    override_usage = "wispdisk.exe /add (/hdd|/rem) /letter <LETTER> /size <SIZE> [/fs <FAT|FAT32|NTFS>]\n       wispdisk.exe /del /letter <LETTER> [/force]\n       wispdisk.exe /del /adapter <INSTANCE> /id <ID> [/force]\n       wispdisk.exe /list [/json]",
    group(ArgGroup::new("operation").required(true).multiple(false).args(["add", "delete", "list"])),
    group(ArgGroup::new("media").multiple(false).args(["hdd", "removable"]))
)]
pub struct Cli {
    /// Create a virtual disk.
    #[arg(long)]
    add: bool,

    /// Remove a virtual disk.
    #[arg(long = "del")]
    delete: bool,

    /// Dismount even if volume locking fails; open files may become invalid and data may be lost (only with /del).
    #[arg(long)]
    force: bool,

    /// List driver allocations, including unformatted and unmounted disks.
    #[arg(long)]
    list: bool,

    /// Return inventory as JSON (only with /list).
    #[arg(long)]
    json: bool,

    /// Stable device ID for deletion without a drive letter.
    #[arg(long)]
    id: Option<u32>,

    /// Adapter instance from /list; required with /id.
    #[arg(long)]
    adapter: Option<u64>,

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

    /// File system for the new volume: FAT, FAT32, or NTFS (default).
    #[arg(long = "fs", value_name = "FAT|FAT32|NTFS")]
    file_system: Option<FileSystem>,

    /// Write CLI and driver diagnostics to stdout (error, warn, info, debug, trace).
    #[arg(long, num_args = 0..=1, default_missing_value = "info", value_name = "LEVEL", conflicts_with = "json")]
    pub log: Option<tracing::level_filters::LevelFilter>,
}

impl Cli {
    pub fn into_command(self) -> Result<Command, ValidationError> {
        if self.force && !self.delete {
            return Err(ValidationError::ForceWithoutDelete);
        }
        if self.list {
            if self.letter.is_some()
                || self.size.is_some()
                || self.file_system.is_some()
                || self.hdd
                || self.removable
                || self.id.is_some()
                || self.adapter.is_some()
            {
                return Err(ValidationError::UnexpectedOptions);
            }
            return Ok(Command::List { json: self.json });
        }
        if self.json || (self.add && (self.id.is_some() || self.adapter.is_some())) {
            return Err(ValidationError::UnexpectedOptions);
        }
        if self.add {
            let letter = self.letter.ok_or(ValidationError::MissingLetter)?;
            let media = match (self.hdd, self.removable) {
                (true, false) => MediaKind::Fixed,
                (false, true) => MediaKind::Removable,
                _ => return Err(ValidationError::MissingMedia),
            };
            let size = self.size.ok_or(ValidationError::MissingSize)?;
            let file_system = self.file_system.unwrap_or_default();
            if file_system == FileSystem::Fat32 && size.0 < MIN_FAT32_DISK_SIZE {
                return Err(ValidationError::Fat32SizeTooSmall);
            }
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
                file_system,
            })
        } else {
            if self.hdd || self.removable {
                return Err(ValidationError::MediaOnDelete);
            }
            if self.size.is_some() {
                return Err(ValidationError::SizeOnDelete);
            }
            if self.file_system.is_some() {
                return Err(ValidationError::FileSystemOnDelete);
            }
            match (self.letter, self.id, self.adapter) {
                (Some(letter), None, None) => Ok(Command::Delete {
                    letter,
                    force: self.force,
                }),
                (None, Some(id), Some(adapter)) if id != 0 && adapter != 0 => {
                    Ok(Command::DeleteId {
                        adapter,
                        id,
                        force: self.force,
                    })
                }
                (None, None, None) => Err(ValidationError::MissingLetter),
                _ => Err(ValidationError::DeleteIdentity),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    List {
        json: bool,
    },
    DeleteId {
        adapter: u64,
        id: u32,
        force: bool,
    },
    Add {
        letter: DriveLetter,
        media: MediaKind,
        size_bytes: u64,
        file_system: FileSystem,
    },
    Delete {
        letter: DriveLetter,
        force: bool,
    },
}

impl fmt::Display for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List { .. } => formatter.write_str("list WispDisk allocations"),
            Self::DeleteId { adapter, id, force } => write!(
                formatter,
                "delete WispDisk device {id} on adapter instance {adapter}{}",
                if *force { " (force)" } else { "" }
            ),
            Self::Add {
                letter,
                media,
                size_bytes,
                file_system,
            } => write!(
                formatter,
                "add {media} disk at {letter} with {size_bytes} bytes formatted as {file_system}"
            ),
            Self::Delete { letter, force } => write!(
                formatter,
                "delete virtual disk at {letter}{}",
                if *force { " (force)" } else { "" }
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FileSystem {
    Fat,
    Fat32,
    #[default]
    Ntfs,
}

impl fmt::Display for FileSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fat => "FAT",
            Self::Fat32 => "FAT32",
            Self::Ntfs => "NTFS",
        })
    }
}

impl FromStr for FileSystem {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_uppercase().as_str() {
            "FAT" => Ok(Self::Fat),
            "FAT32" => Ok(Self::Fat32),
            "NTFS" => Ok(Self::Ntfs),
            _ => Err("file system must be FAT, FAT32, or NTFS".into()),
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
    UnexpectedOptions,
    ForceWithoutDelete,
    DeleteIdentity,
    MissingLetter,
    MissingMedia,
    MissingSize,
    MediaOnDelete,
    SizeOnDelete,
    FileSystemOnDelete,
    SizeTooSmall,
    Fat32SizeTooSmall,
    SizeTooLarge,
    UnalignedSize,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnexpectedOptions => "options are not valid for the selected operation",
            Self::ForceWithoutDelete => "/force is only valid with /del",
            Self::DeleteIdentity => {
                "delete requires either /letter or both nonzero /adapter and /id"
            }
            Self::MissingLetter => "/letter is required",
            Self::MissingMedia => "exactly one of /hdd or /rem is required with /add",
            Self::MissingSize => "/size is required with /add",
            Self::MediaOnDelete => "/hdd and /rem are only valid with /add",
            Self::SizeOnDelete => "/size is only valid with /add",
            Self::FileSystemOnDelete => "/fs is only valid with /add",
            Self::SizeTooSmall => "disk size must be at least 16 MiB",
            Self::Fat32SizeTooSmall => "disk size must be at least 64 MiB for FAT32 formatting",
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
        "list" => "list",
        "json" => "json",
        "id" => "id",
        "adapter" => "adapter",
        "add" => "add",
        "del" => "del",
        "force" => "force",
        "hdd" => "hdd",
        "rem" => "rem",
        "letter" => "letter",
        "size" => "size",
        "fs" => "fs",
        "log" => "log",
        "timings" => return OsString::from("--log=debug"),
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
    #[test]
    fn logging_is_opt_in_and_accepts_levels() {
        use tracing::level_filters::LevelFilter;
        assert_eq!(parse(&["wispdisk.exe", "/list"]).log, None);
        assert_eq!(
            parse(&["wispdisk.exe", "/list", "/log"]).log,
            Some(LevelFilter::INFO)
        );
        assert_eq!(
            parse(&["wispdisk.exe", "/list", "/log:debug"]).log,
            Some(LevelFilter::DEBUG)
        );
        assert_eq!(
            parse(&["wispdisk.exe", "/list", "/log", "trace"]).log,
            Some(LevelFilter::TRACE)
        );
    }

    #[test]
    fn rejects_logs_in_json_output_and_invalid_levels() {
        for args in [
            vec!["wispdisk.exe", "/list", "/json", "/log"],
            vec!["wispdisk.exe", "/list", "/log:invalid"],
        ] {
            assert!(Cli::try_parse_from(normalize_windows_args(args)).is_err());
        }
    }

    #[test]
    fn accepts_inventory_and_identity_deletion() {
        assert_eq!(
            parse(&["wispdisk.exe", "/list", "/json"]).into_command(),
            Ok(Command::List { json: true })
        );
        assert_eq!(
            parse(&["wispdisk.exe", "/del", "/adapter:123", "/id:7"]).into_command(),
            Ok(Command::DeleteId {
                adapter: 123,
                id: 7,
                force: false,
            })
        );
    }

    #[test]
    fn rejects_incomplete_or_conflicting_deletion_identity() {
        for arguments in [
            vec!["wispdisk.exe", "/del", "/id:7"],
            vec!["wispdisk.exe", "/del", "/adapter:123", "/id:0"],
            vec!["wispdisk.exe", "/del", "/adapter:123", "/id:7", "/letter:A"],
        ] {
            assert_eq!(
                parse(&arguments).into_command(),
                Err(ValidationError::DeleteIdentity)
            );
        }
    }

    #[test]
    fn rejects_mutation_options_on_inventory() {
        assert_eq!(
            parse(&["wispdisk.exe", "/list", "/letter:A"]).into_command(),
            Err(ValidationError::UnexpectedOptions)
        );
        assert_eq!(
            parse(&["wispdisk.exe", "/del", "/letter:A", "/json"]).into_command(),
            Err(ValidationError::UnexpectedOptions)
        );
    }
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
                file_system: FileSystem::Ntfs,
            }
        );
    }

    #[test]
    fn accepts_file_system_syntax_and_ignores_value_case() {
        for arguments in [
            &[
                "wispdisk.exe",
                "/add",
                "/hdd",
                "/letter:R",
                "/size:64MiB",
                "/fs:fat32",
            ][..],
            &[
                "wispdisk.exe",
                "/add",
                "/hdd",
                "/letter:R",
                "/size:64MiB",
                "/fs",
                "Fat32",
            ][..],
            &[
                "wispdisk.exe",
                "/add",
                "/hdd",
                "/letter:R",
                "/size:64MiB",
                "--fs",
                "FAT32",
            ][..],
        ] {
            let parsed = parse(arguments);
            assert!(matches!(
                parsed.into_command().unwrap(),
                Command::Add {
                    file_system: FileSystem::Fat32,
                    ..
                }
            ));
        }
    }

    #[test]
    fn rejects_unknown_file_system() {
        let result = Cli::try_parse_from(normalize_windows_args([
            "wispdisk.exe",
            "/add",
            "/hdd",
            "/letter:R",
            "/size:64MiB",
            "/fs:exFAT",
        ]));
        assert!(result.is_err());
    }

    #[test]
    fn accepts_delete_command() {
        let parsed = parse(&["wispdisk.exe", "/del", "/letter", "s:"]);
        assert_eq!(
            parsed.into_command().unwrap(),
            Command::Delete {
                letter: DriveLetter('S'),
                force: false,
            }
        );
    }

    #[test]
    fn accepts_force_for_both_deletion_forms() {
        for force_option in ["/force", "/FORCE", "--force"] {
            assert_eq!(
                parse(&["wispdisk.exe", "/del", "/letter:R", force_option]).into_command(),
                Ok(Command::Delete {
                    letter: DriveLetter('R'),
                    force: true,
                })
            );
            assert_eq!(
                parse(&[
                    "wispdisk.exe",
                    "/del",
                    "/adapter:123",
                    "/id:7",
                    force_option,
                ])
                .into_command(),
                Ok(Command::DeleteId {
                    adapter: 123,
                    id: 7,
                    force: true,
                })
            );
        }
    }

    #[test]
    fn rejects_force_without_deletion() {
        for arguments in [
            vec!["wispdisk.exe", "/list", "/force"],
            vec![
                "wispdisk.exe",
                "/add",
                "/hdd",
                "/letter:R",
                "/size:64MiB",
                "/force",
            ],
        ] {
            assert_eq!(
                parse(&arguments).into_command(),
                Err(ValidationError::ForceWithoutDelete)
            );
        }
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
    fn applies_file_system_specific_minimum_size() {
        for file_system in ["FAT", "NTFS"] {
            let parsed = parse(&[
                "wispdisk.exe",
                "/add",
                "/hdd",
                "/letter:R",
                "/size:16MiB",
                "/fs",
                file_system,
            ]);
            assert!(parsed.into_command().is_ok());
        }

        let parsed = parse(&[
            "wispdisk.exe",
            "/add",
            "/hdd",
            "/letter:R",
            "/size:63MiB",
            "/fs:FAT32",
        ]);
        assert_eq!(
            parsed.into_command(),
            Err(ValidationError::Fat32SizeTooSmall)
        );

        let parsed = parse(&[
            "wispdisk.exe",
            "/add",
            "/hdd",
            "/letter:R",
            "/size:64MiB",
            "/fs:FAT32",
        ]);
        assert!(parsed.into_command().is_ok());

        for file_system in ["FAT", "FAT32", "NTFS"] {
            let parsed = parse(&[
                "wispdisk.exe",
                "/add",
                "/hdd",
                "/letter:R",
                "/size:256MiB",
                "/fs",
                file_system,
            ]);
            assert!(parsed.into_command().is_ok());
        }
    }

    #[test]
    fn displays_selected_file_system() {
        let command = Command::Add {
            letter: DriveLetter('R'),
            media: MediaKind::Fixed,
            size_bytes: 64 * 1024 * 1024,
            file_system: FileSystem::Fat32,
        };
        assert_eq!(
            command.to_string(),
            "add fixed disk at R: with 67108864 bytes formatted as FAT32"
        );
    }

    #[test]
    fn rejects_file_system_on_delete() {
        let parsed = parse(&["wispdisk.exe", "/del", "/letter:R", "/fs:FAT"]);
        assert_eq!(
            parsed.into_command(),
            Err(ValidationError::FileSystemOnDelete)
        );
    }

    #[test]
    fn leaves_unknown_slash_arguments_untouched() {
        let normalized = normalize_windows_args(["wispdisk.exe", "/not-an-option"]);
        assert_eq!(normalized[1], OsString::from("/not-an-option"));
    }

    #[test]
    fn accepts_legacy_timings_alias() {
        let parsed = parse(&[
            "wispdisk.exe",
            "/add",
            "/hdd",
            "/letter:R",
            "/size:64MiB",
            "/timings",
        ]);
        assert_eq!(parsed.log, Some(tracing::level_filters::LevelFilter::DEBUG));
    }
}
