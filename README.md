# WispDisk for Windows

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
SCSI address, creates one MBR partition, formats it as NTFS, and assigns the
requested drive letter. Delete resolves the volume back to that SCSI address,
locks and dismounts it, then removes the matching driver device ID.

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
wispdisk.exe /add /rem /letter:S /size:64MiB
wispdisk.exe /del /letter R
```

Rules enforced now:

- exactly one of `/add` and `/del`;
- exactly one of `/hdd` and `/rem` for `/add`;
- `/letter` is required;
- `/size` is required only for `/add`, is from 16 MiB through 256 MiB, and is
  512-byte aligned;
- size suffixes are `B`, `KB`, `KiB`, `MB`, `MiB`, `GB`, `GiB`, `TB`, and
  `TiB`.

Use the hidden development switch `/dry-run` to exercise validation without
touching a driver:

```powershell
cargo run -p wispdisk-cli -- /add /rem /letter:R /size:64MiB /dry-run
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
`.sys`, `.inf`, and signed `.cat` package.

The full build compiles the driver with Driver Code Analysis, test-signs the
`.sys`, regenerates and signs the catalog, verifies both signatures and catalog
membership, embeds those exact package bytes, signs the final executable, and
verifies its signature and architecture. A SHA-256 manifest is emitted for each
architecture under `artifacts\signing`.

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
