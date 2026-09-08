use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

const PACKAGE_FILES: [&str; 3] = ["WispDisk.sys", "WispDisk.inf", "WispDisk.cat"];

type BuildResult<T> = Result<T, String>;

fn main() {
    if let Err(error) = run_main() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run_main() -> BuildResult<()> {
    if !cfg!(windows) {
        return Err("the WispDisk driver build and signing flow requires Windows".into());
    }

    let options = Options::parse(env::args_os().skip(1))?;
    build(&options)
}

#[derive(Clone, Copy)]
enum Configuration {
    Debug,
    Release,
}

impl Configuration {
    fn parse(value: &OsStr) -> BuildResult<Self> {
        match value.to_string_lossy().to_ascii_lowercase().as_str() {
            "debug" => Ok(Self::Debug),
            "release" => Ok(Self::Release),
            _ => Err(format!(
                "unsupported configuration '{}'; expected Debug or Release",
                value.to_string_lossy()
            )),
        }
    }

    fn as_msbuild(self) -> &'static str {
        match self {
            Self::Debug => "Debug",
            Self::Release => "Release",
        }
    }

    fn cargo_profile(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

#[derive(Clone, Copy)]
enum Platform {
    X64,
    Arm64,
}

impl Platform {
    fn parse(value: &OsStr) -> BuildResult<Self> {
        match value.to_string_lossy().to_ascii_lowercase().as_str() {
            "x64" | "amd64" => Ok(Self::X64),
            "arm64" | "aarch64" => Ok(Self::Arm64),
            _ => Err(format!(
                "unsupported platform '{}'; expected x64 or ARM64",
                value.to_string_lossy()
            )),
        }
    }

    fn as_msbuild(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "ARM64",
        }
    }

    fn rust_target(self) -> &'static str {
        match self {
            Self::X64 => "x86_64-pc-windows-msvc",
            Self::Arm64 => "aarch64-pc-windows-msvc",
        }
    }

    fn inf2cat_os(self) -> &'static str {
        match self {
            Self::X64 => "10_X64",
            Self::Arm64 => "10_RS3_ARM64",
        }
    }

    fn pe_machine(self) -> u16 {
        match self {
            Self::X64 => 0x8664,
            Self::Arm64 => 0xAA64,
        }
    }

    fn pe_machine_name(self) -> &'static str {
        match self {
            Self::X64 => "AMD64",
            Self::Arm64 => "ARM64",
        }
    }

    fn spectre_library_platform(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }

    fn spectre_component(self) -> &'static str {
        match self {
            Self::X64 => "Microsoft.VisualStudio.Component.VC.Runtimes.x86.x64.Spectre",
            Self::Arm64 => "Microsoft.VisualStudio.Component.VC.Runtimes.ARM64.Spectre",
        }
    }
}

struct Options {
    configuration: Configuration,
    platform: Platform,
}

impl Options {
    fn parse<I>(args: I) -> BuildResult<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut args = args.into_iter();
        let command = args
            .next()
            .ok_or_else(usage)?
            .to_string_lossy()
            .to_ascii_lowercase();
        if command != "build" {
            return Err(format!("unsupported command '{command}'\n{}", usage()));
        }

        let mut configuration = None;
        let mut platform = None;
        while let Some(argument) = args.next() {
            match argument.to_string_lossy().as_ref() {
                "--configuration" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--configuration requires a value".to_string())?;
                    configuration = Some(Configuration::parse(&value)?);
                }
                "--platform" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--platform requires a value".to_string())?;
                    platform = Some(Platform::parse(&value)?);
                }
                unknown => return Err(format!("unknown argument '{unknown}'\n{}", usage())),
            }
        }

        Ok(Self {
            configuration: configuration
                .ok_or_else(|| "missing required --configuration option".to_string())?,
            platform: platform.ok_or_else(|| "missing required --platform option".to_string())?,
        })
    }
}

fn usage() -> String {
    "usage: wispdisk-build build --configuration <Debug|Release> --platform <x64|ARM64>".into()
}

fn build(options: &Options) -> BuildResult<()> {
    let repo = find_repo_root()?;
    let driver_solution = repo.join("driver/WispDisk.sln");
    let driver_project = repo.join("driver/WispDisk.vcxproj");
    require_file(&driver_solution, "driver solution")?;
    require_file(&driver_project, "driver project")?;

    confirm_rust_target(options.platform)?;
    let visual_studio = discover_visual_studio()?;
    confirm_spectre_libraries(&visual_studio.installation, options.platform)?;
    let sdk_version = windows_sdk_version(&driver_project)?;
    let sign_tool = find_windows_kit_tool(&sdk_version, "signtool.exe")?;
    let inf2cat = find_windows_kit_tool(&sdk_version, "Inf2Cat.exe")?;
    let certutil = find_on_path("certutil.exe")
        .ok_or_else(|| "certutil.exe was not found on PATH".to_string())?;

    log_step(&format!(
        "Building {}/{} driver",
        options.configuration.as_msbuild(),
        options.platform.as_msbuild()
    ));
    let mut msbuild = Command::new(&visual_studio.msbuild);
    msbuild
        .arg(&driver_solution)
        .args(["/m", "/nologo", "/verbosity:minimal", "/t:Build"])
        .arg(format!(
            "/p:Configuration={}",
            options.configuration.as_msbuild()
        ))
        .arg(format!("/p:Platform={}", options.platform.as_msbuild()))
        .arg("/p:RunCodeAnalysis=true");
    run(&mut msbuild, "driver build")?;

    let driver_output = repo
        .join("artifacts/driver")
        .join(options.configuration.as_msbuild())
        .join(options.platform.as_msbuild());
    let built_package = driver_output.join("WispDisk");
    let built_driver = built_package.join("WispDisk.sys");
    confirm_pe_architecture(&built_driver, options.platform)?;

    let package_root = repo.join("artifacts/package");
    let package_dir = package_root
        .join(options.configuration.as_msbuild())
        .join(options.platform.as_msbuild());
    reset_directory(&package_root, &package_dir)?;
    for file_name in PACKAGE_FILES {
        copy_required_file(
            &built_package.join(file_name),
            &package_dir.join(file_name),
            "driver build output",
        )?;
    }

    let signing_dir = repo.join("artifacts/signing");
    fs::create_dir_all(&signing_dir)
        .map_err(|error| path_error("create signing directory", &signing_dir, error))?;
    let certificate_path = signing_dir.join("WispDiskTest.cer");
    let driver_certificate = prepare_driver_certificate(
        &repo,
        &certutil,
        &certificate_path,
        env::var_os("WISPDISK_DRIVER_CERT_THUMBPRINT"),
    )?;

    let driver_path = package_dir.join("WispDisk.sys");
    let inf_path = package_dir.join("WispDisk.inf");
    let catalog_path = package_dir.join("WispDisk.cat");
    confirm_pe_architecture(&driver_path, options.platform)?;

    log_step("Test-signing the driver binary");
    sign_file(
        &sign_tool,
        &driver_certificate,
        &driver_path,
        "WispDisk virtual storage driver",
    )?;

    if catalog_path.exists() {
        fs::remove_file(&catalog_path)
            .map_err(|error| path_error("remove stale driver catalog", &catalog_path, error))?;
    }
    let mut create_catalog = Command::new(&inf2cat);
    create_catalog
        .arg(format!("/driver:{}", package_dir.display()))
        .arg(format!("/os:{}", options.platform.inf2cat_os()))
        .arg("/uselocaltime");
    run(&mut create_catalog, "driver catalog generation")?;
    require_file(&catalog_path, "regenerated driver catalog")?;

    log_step("Signing the regenerated driver catalog");
    sign_file(
        &sign_tool,
        &driver_certificate,
        &catalog_path,
        "WispDisk driver package",
    )?;
    verify_signature(&sign_tool, &driver_path, "driver signature")?;
    verify_signature(&sign_tool, &catalog_path, "catalog signature")?;
    verify_catalog_member(&sign_tool, &catalog_path, &driver_path)?;
    verify_catalog_member(&sign_tool, &catalog_path, &inf_path)?;

    log_step("Embedding the signed driver package into the Rust CLI");
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(&repo)
        .args([
            "build",
            "--locked",
            "--package",
            "wispdisk-cli",
            "--target",
            options.platform.rust_target(),
        ])
        .env("WISPDISK_DRIVER_PACKAGE_DIR", &package_dir)
        .env_remove("CARGO_TARGET_DIR");
    if matches!(options.configuration, Configuration::Release) {
        cargo.arg("--release");
    }
    run(&mut cargo, "Rust CLI build")?;

    let cargo_cli = repo
        .join("target")
        .join(options.platform.rust_target())
        .join(options.configuration.cargo_profile())
        .join("wispdisk.exe");
    confirm_pe_architecture(&cargo_cli, options.platform)?;

    let final_dir = repo
        .join("artifacts/bin")
        .join(options.configuration.as_msbuild())
        .join(options.platform.as_msbuild());
    fs::create_dir_all(&final_dir)
        .map_err(|error| path_error("create final artifact directory", &final_dir, error))?;
    let final_cli = final_dir.join("wispdisk.exe");
    copy_required_file(&cargo_cli, &final_cli, "Rust CLI")?;

    let exe_certificate = match env::var_os("WISPDISK_EXE_CERT_THUMBPRINT") {
        Some(value) => normalize_thumbprint(&value)?,
        None => driver_certificate.clone(),
    };
    log_step("Signing the final executable");
    sign_file(
        &sign_tool,
        &exe_certificate,
        &final_cli,
        "WispDisk command-line utility",
    )?;
    verify_signature(&sign_tool, &final_cli, "executable signature")?;
    confirm_pe_architecture(&final_cli, options.platform)?;
    confirm_package_embedded(&final_cli, &package_dir)?;

    let pdb_path = driver_output.join("WispDisk.pdb");
    require_file(&pdb_path, "driver symbols")?;
    let manifest_path = signing_dir.join(format!(
        "{}-{}-manifest.json",
        options.configuration.as_msbuild(),
        options.platform.as_msbuild()
    ));
    write_manifest(
        &certutil,
        &manifest_path,
        options,
        &sdk_version,
        &driver_certificate,
        &exe_certificate,
        &[
            ("Driver", &driver_path),
            ("DriverInf", &inf_path),
            ("DriverCatalog", &catalog_path),
            ("DriverSymbols", &pdb_path),
            ("SignedCli", &final_cli),
            ("PublicTestCertificate", &certificate_path),
        ],
    )?;

    log_step("Build completed successfully");
    println!("Signed executable: {}", final_cli.display());
    println!("Embedded package:  {}", package_dir.display());
    println!("Build manifest:    {}", manifest_path.display());
    Ok(())
}

struct VisualStudio {
    installation: PathBuf,
    msbuild: PathBuf,
}

fn discover_visual_studio() -> BuildResult<VisualStudio> {
    let standard = env::var_os("ProgramFiles(x86)")
        .map(|root| PathBuf::from(root).join("Microsoft Visual Studio/Installer/vswhere.exe"));
    let vswhere = standard
        .filter(|path| path.is_file())
        .or_else(|| find_on_path("vswhere.exe"))
        .ok_or_else(|| {
            "Visual Studio Installer vswhere.exe was not found; install Visual Studio 2022 with Desktop development with C++"
                .to_string()
        })?;

    let mut command = Command::new(&vswhere);
    command.args([
        "-latest",
        "-products",
        "*",
        "-requires",
        "Microsoft.Component.MSBuild",
        "Component.Microsoft.Windows.DriverKit",
        "-property",
        "installationPath",
    ]);
    let output = capture(&mut command, "Visual Studio discovery")?;
    let installation = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "Visual Studio 2022 with MSBuild and the Windows Driver Kit component was not found"
                .to_string()
        })?;
    if !installation.is_dir() {
        return Err(format!(
            "Visual Studio discovery returned a missing directory: {}",
            installation.display()
        ));
    }

    let msbuild = [
        installation.join("MSBuild/Current/Bin/amd64/MSBuild.exe"),
        installation.join("MSBuild/Current/Bin/MSBuild.exe"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| {
        format!(
            "MSBuild.exe was not found under '{}'",
            installation.display()
        )
    })?;

    Ok(VisualStudio {
        installation,
        msbuild,
    })
}

fn confirm_spectre_libraries(visual_studio: &Path, platform: Platform) -> BuildResult<()> {
    let version_file =
        visual_studio.join("VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt");
    require_file(&version_file, "active MSVC toolset version file")?;
    let version = fs::read_to_string(&version_file)
        .map_err(|error| path_error("read MSVC toolset version", &version_file, error))?;
    let spectre_dir = visual_studio
        .join("VC/Tools/MSVC")
        .join(version.trim())
        .join("lib/spectre")
        .join(platform.spectre_library_platform());
    let has_library = fs::read_dir(&spectre_dir)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry.path().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("lib"))
            })
        })
        .unwrap_or(false);
    if !has_library {
        return Err(format!(
            "Spectre-mitigated {} libraries are missing; install Visual Studio component '{}'",
            platform.as_msbuild(),
            platform.spectre_component()
        ));
    }
    Ok(())
}

fn confirm_rust_target(platform: Platform) -> BuildResult<()> {
    let mut command = Command::new("rustup");
    command.args(["target", "list", "--installed"]);
    let output = capture(&mut command, "installed Rust target discovery")?;
    let installed = String::from_utf8_lossy(&output.stdout);
    if !installed
        .lines()
        .any(|line| line.trim() == platform.rust_target())
    {
        return Err(format!(
            "Rust target '{}' is not installed; run: rustup target add {}",
            platform.rust_target(),
            platform.rust_target()
        ));
    }
    Ok(())
}

fn windows_sdk_version(project: &Path) -> BuildResult<String> {
    let contents = fs::read_to_string(project)
        .map_err(|error| path_error("read driver project", project, error))?;
    let open = "<WindowsTargetPlatformVersion>";
    let close = "</WindowsTargetPlatformVersion>";
    let mut versions = Vec::new();
    let mut remainder = contents.as_str();
    while let Some(start) = remainder.find(open) {
        remainder = &remainder[start + open.len()..];
        let end = remainder.find(close).ok_or_else(|| {
            format!(
                "unterminated WindowsTargetPlatformVersion in '{}'",
                project.display()
            )
        })?;
        let value = remainder[..end].trim();
        if !value.is_empty() && !versions.iter().any(|existing| existing == value) {
            versions.push(value.to_string());
        }
        remainder = &remainder[end + close.len()..];
    }
    if versions.len() != 1 {
        return Err(format!(
            "expected exactly one WindowsTargetPlatformVersion in '{}'",
            project.display()
        ));
    }
    Ok(versions.remove(0))
}

fn find_windows_kit_tool(version: &str, name: &str) -> BuildResult<PathBuf> {
    let kits_root = windows_kits_root();
    let version_dir = kits_root.join("bin").join(version);
    if !version_dir.is_dir() {
        return Err(format!(
            "Windows Kit {version} was not found under '{}'",
            kits_root.display()
        ));
    }

    let preferences: &[&str] = if cfg!(target_arch = "aarch64") {
        &["arm64", "x64", "x86"]
    } else {
        &["x64", "x86", "arm64"]
    };
    preferences
        .iter()
        .map(|architecture| version_dir.join(architecture).join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("'{name}' was not found in Windows Kit {version}"))
}

fn windows_kits_root() -> PathBuf {
    if let Some(path) = env::var_os("WISPDISK_WINDOWS_KITS_ROOT") {
        return PathBuf::from(path);
    }

    if let Some(reg) = find_on_path("reg.exe") {
        let mut query = Command::new(reg);
        query.args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots",
            "/v",
            "KitsRoot10",
        ]);
        if let Ok(output) = query.output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Some((_, value)) = line.split_once("REG_SZ") {
                        let path = PathBuf::from(value.trim());
                        if path.is_dir() {
                            return path;
                        }
                    }
                }
            }
        }
    }

    env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
        .join("Windows Kits/10")
}

fn prepare_driver_certificate(
    repo: &Path,
    certutil: &Path,
    certificate_path: &Path,
    requested_thumbprint: Option<OsString>,
) -> BuildResult<String> {
    let requested_thumbprint = requested_thumbprint
        .as_deref()
        .map(normalize_thumbprint)
        .transpose()?;

    if let Some(thumbprint) = requested_thumbprint.as_ref() {
        log_step("Exporting the requested driver test certificate");
        let mut export = Command::new(certutil);
        export
            .args(["-f", "-user", "-store", "My"])
            .arg(thumbprint)
            .arg(certificate_path);
        run(&mut export, "test certificate export")?;
    } else if !certificate_path.is_file() {
        log_step("Creating a non-exportable current-user test certificate");
        let request = repo.join("scripts/WispDiskTestCertificate.inf");
        require_file(&request, "test certificate request")?;
        let certreq = find_on_path("certreq.exe")
            .ok_or_else(|| "certreq.exe was not found on PATH".to_string())?;
        let mut create = Command::new(certreq);
        create.arg("-new").arg(&request).arg(certificate_path);
        run(&mut create, "test certificate creation")?;
    }

    require_file(certificate_path, "public test certificate")?;
    let actual_thumbprint = hash_file(certutil, certificate_path, "SHA1", 40)?;
    if let Some(expected) = requested_thumbprint {
        if actual_thumbprint != expected {
            return Err(format!(
                "exported certificate thumbprint {actual_thumbprint} did not match requested thumbprint {expected}"
            ));
        }
    }

    if !env_flag("WISPDISK_SKIP_CERTIFICATE_TRUST")? {
        log_step("Trusting the public test certificate for the current user");
        for store in ["Root", "TrustedPublisher"] {
            let mut trust = Command::new(certutil);
            trust
                .args(["-f", "-user", "-addstore", store])
                .arg(certificate_path);
            run(&mut trust, &format!("add certificate to {store} store"))?;
        }
    }

    Ok(actual_thumbprint)
}

fn env_flag(name: &str) -> BuildResult<bool> {
    match env::var_os(name) {
        None => Ok(false),
        Some(value) => match value.to_string_lossy().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(true),
            "0" | "false" | "no" | "" => Ok(false),
            unknown => Err(format!(
                "environment variable {name} must be true or false, not '{unknown}'"
            )),
        },
    }
}

fn normalize_thumbprint(value: &OsStr) -> BuildResult<String> {
    let normalized: String = value
        .to_string_lossy()
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .map(|character| character.to_ascii_uppercase())
        .collect();
    if normalized.len() != 40
        || !normalized
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(format!(
            "certificate thumbprint '{}' must contain exactly 40 hexadecimal characters",
            value.to_string_lossy()
        ));
    }
    Ok(normalized)
}

fn sign_file(
    sign_tool: &Path,
    certificate_thumbprint: &str,
    path: &Path,
    description: &str,
) -> BuildResult<()> {
    require_file(path, "signing input")?;
    let mut command = Command::new(sign_tool);
    command
        .args(["sign", "/v", "/fd", "SHA256", "/s", "My", "/sha1"])
        .arg(certificate_thumbprint)
        .args(["/d", description]);
    if let Some(timestamp_url) = env::var_os("WISPDISK_TIMESTAMP_URL") {
        if !timestamp_url.is_empty() {
            command.args([
                "/tr",
                timestamp_url.to_string_lossy().as_ref(),
                "/td",
                "SHA256",
            ]);
        }
    }
    command.arg(path);
    run(&mut command, &format!("sign '{}'", path.display()))
}

fn verify_signature(sign_tool: &Path, path: &Path, description: &str) -> BuildResult<()> {
    let mut command = Command::new(sign_tool);
    command.args(["verify", "/v", "/pa"]).arg(path);
    run(&mut command, description)
}

fn verify_catalog_member(sign_tool: &Path, catalog: &Path, member: &Path) -> BuildResult<()> {
    let mut command = Command::new(sign_tool);
    command
        .args(["verify", "/v", "/pa", "/c"])
        .arg(catalog)
        .arg(member);
    run(
        &mut command,
        &format!("catalog membership for '{}'", member.display()),
    )
}

fn confirm_package_embedded(executable: &Path, package_dir: &Path) -> BuildResult<()> {
    let executable_bytes = fs::read(executable)
        .map_err(|error| path_error("read signed executable", executable, error))?;
    for file_name in PACKAGE_FILES {
        let package_path = package_dir.join(file_name);
        let package_bytes = fs::read(&package_path)
            .map_err(|error| path_error("read embedded package input", &package_path, error))?;
        if package_bytes.is_empty() || !contains_bytes(&executable_bytes, &package_bytes) {
            return Err(format!(
                "signed executable does not contain the exact signed bytes from '{}'",
                package_path.display()
            ));
        }
    }
    Ok(())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn confirm_pe_architecture(path: &Path, platform: Platform) -> BuildResult<()> {
    let actual = pe_machine(path)?;
    if actual != platform.pe_machine() {
        return Err(format!(
            "architecture mismatch for '{}': expected {} (0x{:04X}), found 0x{actual:04X}",
            path.display(),
            platform.pe_machine_name(),
            platform.pe_machine()
        ));
    }
    Ok(())
}

fn pe_machine(path: &Path) -> BuildResult<u16> {
    require_file(path, "PE image")?;
    let mut file =
        fs::File::open(path).map_err(|error| path_error("open PE image", path, error))?;
    let length = file
        .metadata()
        .map_err(|error| path_error("inspect PE image", path, error))?
        .len();
    if length < 64 {
        return Err(format!(
            "file is too short to be a PE image: {}",
            path.display()
        ));
    }

    let mut word = [0_u8; 2];
    file.read_exact(&mut word)
        .map_err(|error| path_error("read DOS signature", path, error))?;
    if u16::from_le_bytes(word) != 0x5A4D {
        return Err(format!("file is not a valid PE image: {}", path.display()));
    }

    file.seek(SeekFrom::Start(0x3C))
        .map_err(|error| path_error("seek to PE offset", path, error))?;
    let mut dword = [0_u8; 4];
    file.read_exact(&mut dword)
        .map_err(|error| path_error("read PE offset", path, error))?;
    let pe_offset = u32::from_le_bytes(dword) as u64;
    if pe_offset > length.saturating_sub(6) {
        return Err(format!(
            "PE header offset is outside the file: {}",
            path.display()
        ));
    }

    file.seek(SeekFrom::Start(pe_offset))
        .map_err(|error| path_error("seek to PE header", path, error))?;
    file.read_exact(&mut dword)
        .map_err(|error| path_error("read PE signature", path, error))?;
    if u32::from_le_bytes(dword) != 0x0000_4550 {
        return Err(format!("PE signature is invalid: {}", path.display()));
    }
    file.read_exact(&mut word)
        .map_err(|error| path_error("read PE machine", path, error))?;
    Ok(u16::from_le_bytes(word))
}

fn reset_directory(root: &Path, directory: &Path) -> BuildResult<()> {
    if !root.is_absolute() || !directory.is_absolute() || directory.parent() == Some(root) {
        return Err(format!(
            "refusing to reset invalid package directory '{}'",
            directory.display()
        ));
    }
    directory.strip_prefix(root).map_err(|_| {
        format!(
            "refusing to reset package directory outside '{}': {}",
            root.display(),
            directory.display()
        )
    })?;
    if directory.exists() {
        log_step(&format!(
            "Resetting package directory {}",
            directory.display()
        ));
        fs::remove_dir_all(directory)
            .map_err(|error| path_error("reset package directory", directory, error))?;
    }
    fs::create_dir_all(directory)
        .map_err(|error| path_error("create package directory", directory, error))
}

fn write_manifest(
    certutil: &Path,
    path: &Path,
    options: &Options,
    sdk_version: &str,
    driver_certificate: &str,
    exe_certificate: &str,
    files: &[(&str, &PathBuf)],
) -> BuildResult<()> {
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
        .as_secs();
    let mut file_entries = Vec::with_capacity(files.len());
    for (role, file_path) in files {
        require_file(file_path, &format!("manifest input '{role}'"))?;
        let metadata = fs::metadata(file_path)
            .map_err(|error| path_error("inspect manifest input", file_path, error))?;
        let hash = hash_file(certutil, file_path, "SHA256", 64)?;
        file_entries.push(format!(
            "    {{\n      \"Role\": \"{}\",\n      \"Path\": \"{}\",\n      \"Length\": {},\n      \"SHA256\": \"{}\"\n    }}",
            json_escape(role),
            json_escape(&file_path.display().to_string()),
            metadata.len(),
            hash
        ));
    }

    let manifest = format!(
        "{{\n  \"SchemaVersion\": 2,\n  \"CreatedAtUnixSeconds\": {created},\n  \"Configuration\": \"{}\",\n  \"Platform\": \"{}\",\n  \"RustTarget\": \"{}\",\n  \"PeMachine\": \"0x{:04X}\",\n  \"WindowsKitVersion\": \"{}\",\n  \"DriverTestCertificateThumbprint\": \"{}\",\n  \"ExecutableCertificateThumbprint\": \"{}\",\n  \"Files\": [\n{}\n  ]\n}}\n",
        options.configuration.as_msbuild(),
        options.platform.as_msbuild(),
        options.platform.rust_target(),
        options.platform.pe_machine(),
        json_escape(sdk_version),
        driver_certificate,
        exe_certificate,
        file_entries.join(",\n")
    );
    fs::write(path, manifest).map_err(|error| path_error("write build manifest", path, error))
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04X}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn hash_file(
    certutil: &Path,
    path: &Path,
    algorithm: &str,
    expected_length: usize,
) -> BuildResult<String> {
    let mut command = Command::new(certutil);
    command.arg("-hashfile").arg(path).arg(algorithm);
    let output = capture(
        &mut command,
        &format!("{algorithm} hash for '{}'", path.display()),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_digest(&stdout, expected_length).ok_or_else(|| {
        format!(
            "certutil did not return a {expected_length}-character {algorithm} digest for '{}'",
            path.display()
        )
    })
}

fn parse_digest(output: &str, expected_length: usize) -> Option<String> {
    output.lines().find_map(|line| {
        let candidate: String = line
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect();
        (candidate.len() == expected_length
            && candidate
                .chars()
                .all(|character| character.is_ascii_hexdigit()))
        .then(|| candidate.to_ascii_uppercase())
    })
}

fn find_repo_root() -> BuildResult<PathBuf> {
    let starting_points = env::var_os("CARGO_MAKE_WORKING_DIRECTORY")
        .map(PathBuf::from)
        .into_iter()
        .chain(env::current_dir().ok());
    for starting_point in starting_points {
        for candidate in starting_point.ancestors() {
            if candidate.join("Cargo.toml").is_file()
                && candidate.join("driver/WispDisk.vcxproj").is_file()
            {
                return Ok(candidate.to_path_buf());
            }
        }
    }
    Err("could not find the WispDisk repository root".into())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn require_file(path: &Path, description: &str) -> BuildResult<()> {
    let metadata = fs::metadata(path)
        .map_err(|error| path_error(&format!("locate {description}"), path, error))?;
    if !metadata.is_file() {
        return Err(format!("{description} is not a file: {}", path.display()));
    }
    if metadata.len() == 0 {
        return Err(format!("{description} is empty: {}", path.display()));
    }
    Ok(())
}

fn copy_required_file(source: &Path, destination: &Path, description: &str) -> BuildResult<()> {
    require_file(source, description)?;
    fs::copy(source, destination).map_err(|error| {
        format!(
            "failed to copy {} '{}' to '{}': {error}",
            description,
            source.display(),
            destination.display()
        )
    })?;
    require_file(destination, description)
}

fn run(command: &mut Command, description: &str) -> BuildResult<()> {
    let display = format!("{command:?}");
    let status = command
        .status()
        .map_err(|error| format!("failed to start {description} ({display}): {error}"))?;
    if !status.success() {
        return Err(format!(
            "{description} failed with exit code {} ({display})",
            status
                .code()
                .map_or_else(|| "unknown".into(), |code| code.to_string())
        ));
    }
    Ok(())
}

fn capture(command: &mut Command, description: &str) -> BuildResult<Output> {
    let display = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| format!("failed to start {description} ({display}): {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{description} failed with exit code {} ({display})\n{}",
            output
                .status
                .code()
                .map_or_else(|| "unknown".into(), |code| code.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

fn log_step(message: &str) {
    println!("[WispDisk] {message}");
}

fn path_error(action: &str, path: &Path, error: std::io::Error) -> String {
    format!("failed to {action} '{}': {error}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_platform_names() {
        assert!(matches!(
            Platform::parse(OsStr::new("x64")),
            Ok(Platform::X64)
        ));
        assert!(matches!(
            Platform::parse(OsStr::new("aarch64")),
            Ok(Platform::Arm64)
        ));
    }

    #[test]
    fn normalizes_certificate_thumbprints() {
        let input = OsStr::new("00 11 22 33 44 55 66 77 88 99 aa bb cc dd ee ff 00 11 22 33");
        assert_eq!(
            normalize_thumbprint(input).unwrap(),
            "00112233445566778899AABBCCDDEEFF00112233"
        );
        assert!(normalize_thumbprint(OsStr::new("not-a-thumbprint")).is_err());
    }

    #[test]
    fn parses_localized_certutil_digest_output() {
        let output = "localized heading\n00 11 aa BB\nlocalized footer\n";
        assert_eq!(parse_digest(output, 8).as_deref(), Some("0011AABB"));
    }

    #[test]
    fn detects_exact_embedded_bytes() {
        assert!(contains_bytes(
            b"prefix-signed-driver-suffix",
            b"signed-driver"
        ));
        assert!(!contains_bytes(
            b"prefix-signed-driver-suffix",
            b"unsigned-driver"
        ));
        assert!(!contains_bytes(b"anything", b""));
    }

    #[test]
    fn escapes_json_paths() {
        assert_eq!(
            json_escape("C:\\path\n\"file\""),
            "C:\\\\path\\n\\\"file\\\""
        );
    }
}
