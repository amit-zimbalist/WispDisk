# WispDisk for Windows

<p align="center">
  <img src="resources/logo.jpeg" alt="WispDisk logo" width="420">
</p>

This repository contains a VM-testable alpha of a Windows 10+ volatile virtual
disk utility:

- `wispdisk.exe` is a Rust CLI using `clap`.
- `WispDisk.sys` is a C++ StorPort virtual miniport built with MSVC and the WDK.
- `shared/wispdisk_protocol.h` defines the versioned CLI/driver ABI.
- the `.sys`, `.inf`, and `.cat` driver package is embedded into the CLI at
  build time.

The driver exposes one SCSI target with up to 16 dynamic LUNs. It implements the
management protocol, bounded nonpaged RAM backing, the SCSI discovery/capacity/
mode-sense commands needed by the disk stack, and READ/WRITE 6/10/12/16. The CLI
installs a root-enumerated adapter, creates a LUN, safely identifies its exact
SCSI address, creates one MBR partition, formats it as FAT, FAT32, or NTFS, and
assigns the requested drive letter. Delete resolves the volume back to that
SCSI address, locks and dismounts it, then removes the matching driver device
ID.

This is still an **alpha kernel driver**. Build and static analysis are clean,
but it must not be loaded on a workstation. Runtime validation belongs in a
checkpointed Hyper-V guest with kernel dumps and Driver Verifier configured.

## Why StorPort and MSVC

A StorPort virtual miniport lets the standard Windows disk class driver sit
above this driver. That is a much better fit than independently reproducing all
disk-class behavior. Microsoft documents StorPort's virtual miniport interface
for storage devices with no physical hardware association:
<https://learn.microsoft.com/windows-hardware/drivers/storage/overview-of-storage-virtual-miniport-drivers>.

Use **MSVC as the primary driver compiler**. The WDK, Visual Studio driver
projects, INF/catalog tools, Code Analysis for Drivers, Static Driver Verifier,
deployment, signing, and debugging all follow that path. C++ is restricted to
the kernel-safe subset: no exceptions, RTTI, STL ownership, or implicit runtime
allocation. MSVC's `/kernel` mode enforces important parts of that subset:
<https://learn.microsoft.com/cpp/build/reference/kernel-create-kernel-mode-binary>.

`clang-cl` can become a useful second CI compiler later, but should not be the
reference build until the MSVC/WDK build, package, and verifier runs are stable.

## CLI shape

The requested Windows slash syntax is accepted, as are normal `--long` options:

```powershell
wispdisk.exe /add /hdd /letter R /size 128MiB
wispdisk.exe /add /rem /letter:S /size:64MiB /fs:FAT32
wispdisk.exe /add /hdd /letter T /size 32MiB /fs FAT
wispdisk.exe /del /letter R
```

Rules enforced now:

- exactly one of `/add` and `/del`;
- exactly one of `/hdd` and `/rem` for `/add`;
- `/letter` is required;
- `/size` is required only for `/add`, is from 16 MiB through 256 MiB, and is
  512-byte aligned;
- `/fs` is valid only for `/add`, accepts `FAT`, `FAT32`, or `NTFS`
  case-insensitively, and defaults to `NTFS`;
- FAT32 disks must be at least 64 MiB; FAT and NTFS retain the 16 MiB minimum;
- NTFS volumes use the label `WispDisk-XXXXXXXX`; FAT and FAT32 use the
  11-character label `WD-XXXXXXXX`, where `XXXXXXXX` is the device ID;
- size suffixes are `B`, `KB`, `KiB`, `MB`, `MiB`, `GB`, `GiB`, `TB`, and
  `TiB`.

Use the hidden development switch `/dry-run` to exercise validation without
touching a driver:

```powershell
cargo run -p wispdisk-cli -- /add /rem /letter:R /size:64MiB /dry-run
```

Use the hidden `/timings` switch in a disposable test VM to print elapsed time
for adapter discovery, backing-store allocation/zeroing, PnP discovery, each
storage-provisioning phase, filesystem formatting, and final identity
verification:

```powershell
wispdisk.exe /add /hdd /letter R /size 128MiB /timings
```

## Repository layout

```text
cli/                       Rust command line program and embedded package
driver/                    MSVC/WDK StorPort miniport project and INF
shared/                    Versioned fixed-layout driver protocol
docs/architecture.md       Intended storage and control flow
docs/testing.md            VM, Driver Verifier, and soak-test gates
Makefile.toml              cargo-make entry points for x64 and ARM64
scripts/build-tool/        Rust implementation of build/sign/verify steps
```

## Prerequisites

- Rust 1.85 or newer.
- `cargo-make` (`cargo install cargo-make`).
- The explicit Rust MSVC targets must be installed for the architectures being
  built: `rustup target add x86_64-pc-windows-msvc
  aarch64-pc-windows-msvc`.
- Visual Studio 2022 with Desktop development with C++.
- A Windows 11 WDK compatible with the installed Visual Studio/SDK. For a
  VS 2022 environment, WDK 10.0.26100.x is the appropriate supported line.
- MSVC v143 Spectre-mitigated libraries for x86/x64
  (`Microsoft.VisualStudio.Component.VC.Runtimes.x86.x64.Spectre`). ARM64
  builds also require the ARM64/ARM64EC Spectre-mitigated libraries
  (`Microsoft.VisualStudio.Component.VC.Runtimes.ARM64.Spectre`).
- A disposable Hyper-V Windows 10/11 VM for every load and verifier run.

The standard solution build requires both the WDK files under Windows Kits and
the Visual Studio **Windows Driver Kit** component
(`Component.Microsoft.Windows.DriverKit`). Installing only the headers,
libraries, and command-line tools is not enough for the
`WindowsKernelModeDriver10.0` MSBuild platform.

The signed build creates a non-exportable SHA-256 test code-signing certificate
in `Cert:\CurrentUser\My` when `artifacts\signing\WispDiskTest.cer` does not
exist. Only the public certificate is written to the artifacts directory. Never
put a private signing key in the repository or embed one into the executable.

## Build

Safe user-mode checks:

```console
cargo make check
```

Signed release builds after installing the WDK and its Visual Studio component:

```console
cargo make build
cargo make build-x64
cargo make build-arm64
cargo make build-debug
```

`cargo make build` is the default and produces both release architectures. The
final executables are `artifacts\bin\Release\x64\wispdisk.exe` and
`artifacts\bin\Release\ARM64\wispdisk.exe`. Each contains its matching signed
`.sys`, `.inf`, and signed `.cat` package. The executable also embeds the
WispDisk application icon used by Windows Explorer.

The full build compiles the driver with Driver Code Analysis, test-signs the
`.sys`, regenerates and signs the catalog, verifies both signatures and catalog
membership, embeds those exact package bytes, signs the final executable, and
verifies its signature and architecture. The CLI and driver PDBs are preserved
with the final build outputs. A SHA-256 manifest is emitted for each
architecture under `artifacts\signing`.

## Package and publish a release

Build, sign, and package both release architectures locally with:

```console
powershell -File ./scripts/create-release.ps1 -Version v0.1.0
```

This writes `wispdisk-v0.1.0-windows-x64.zip`,
`wispdisk-v0.1.0-windows-arm64.zip`, and `SHA256SUMS.txt` under
`artifacts\release`. Each ZIP contains the signed CLI and driver package, both
PDBs, the build manifest, and the public `.cer` certificate. The packager checks
every input against its build manifest and verifies that the certificate
thumbprint matches both signer thumbprints recorded by the verified build
before creating the ZIPs. Only the public certificate is packaged; the private
key is never exported. Packaging also fails if the certificate file reports
that it contains a private key.

If both architectures are already built, the packaging-only command is:

```console
powershell -File ./scripts/package-release.ps1 -Version v0.1.0
```

Create and push the release tag, then publish the three generated files on the
repository's **Releases** page:

```console
git tag -a v0.1.0 -m "WispDisk v0.1.0"
git push origin v0.1.0
gh release create v0.1.0 artifacts/release/wispdisk-v0.1.0-windows-x64.zip artifacts/release/wispdisk-v0.1.0-windows-arm64.zip artifacts/release/SHA256SUMS.txt --verify-tag --generate-notes
```

The final command is optional: the same tag and three files can be selected in
GitHub's **Draft a new release** web form. Building and signing intentionally
remain local because the required WDK installation and signing certificate are
not provisioned in CI.

The resulting release is test-signed and intended only for development and
disposable test VMs. A public production release of a modern Windows kernel
driver requires the appropriate Microsoft driver-signing process; attaching a
self-signed certificate to a GitHub Release does not make the driver trusted by
Windows.

By default the final executable uses the same disposable test certificate as
the driver. Set `WISPDISK_EXE_CERT_THUMBPRINT` to use a different code-signing
certificate from `Cert:\CurrentUser\My`; set
`WISPDISK_DRIVER_CERT_THUMBPRINT` to select an existing driver test certificate.
Set `WISPDISK_TIMESTAMP_URL` to add an RFC 3161 timestamp. The build never reads
or writes a PFX file.

The generated certificate is added to the current user's Root and
TrustedPublisher stores so SignTool can validate it. Set
`WISPDISK_SKIP_CERTIFICATE_TRUST=true` only when trust is managed separately.
The generated certificate is for disposable test systems and VMs only; it is
not a production-trusted driver signature.

## Signing and testing

A modern x64 or ARM64 Windows guest still needs a signature policy that permits
the test-signed development driver. Do not change boot policy on a development
workstation. Follow the VM workflow in [docs/testing.md](docs/testing.md).

Driver Verifier is a test gate, not a one-time checkbox. It can intentionally
crash Windows when it finds a violation, so Microsoft recommends running it
only on test/debug systems:
<https://learn.microsoft.com/windows-server/administration/windows-commands/verifier>.

## Important scope distinction

`/rem` sets the SCSI INQUIRY removable-media bit. The device behaves like a
removable logical disk, but it does not claim to be attached to a USB bus.
Software that specifically requires `BusTypeUsb`, USB descriptors, or USB
plug/unplug events needs a different and substantially larger virtual USB
design.
