# Test and Driver Verifier plan

Kernel testing begins only after the WDK build is warning-clean and the VM has a
checkpoint. Driver Verifier can deliberately bug-check the target; never enable
it for this driver on the host workstation.

## VM baseline

- Generation 2 Hyper-V VM with Windows 10 22H2 and a currently serviced Windows
  11 image as separate test targets.
- Secure Boot configuration compatible with the chosen test-signing method.
- Kernel dump enabled and a page file large enough to capture it.
- WinDbg network kernel debugging configured before the first driver load.
- A clean checkpoint named `pre-wispdisk`.
- Build artifacts copied into the guest; never build/load from a synced folder.

## Per-build static gates

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
msbuild driver\WispDisk.sln /m /p:Configuration=Debug /p:Platform=x64 /p:RunCodeAnalysis=true
InfVerif.exe /w driver\WispDisk.inf
```

The MSBuild project runs the WDK `DriverMinimumRules` analysis and treats C++
warnings as errors. Callback role declarations and SAL annotations must remain
in place so those tools can reason about the entry points. Static Driver
Verifier is an additional gate once a supported rule set for this StorPort
miniport is selected; it does not replace runtime Driver Verifier.

## Signing in the guest

Use a dedicated, disposable test certificate. Sign the catalog, install only
the public certificate into the guest's required machine stores, enable test
mode if the VM policy requires it, and reboot. Do not commit `.pfx`, `.cer`,
private keys, generated catalogs, or signed binaries.

Microsoft's test-signing overview is here:
<https://learn.microsoft.com/windows-hardware/drivers/install/test-signing>.

## Verifier cycle

From an elevated console in the VM:

```powershell
verifier /reset
verifier /standard /driver WispDisk.sys
shutdown /r /t 0
```

After reboot, confirm the active settings before testing:

```powershell
verifier /querysettings
```

Run each scenario under Verifier, then inspect the kernel log and dump state:

- load/unload adapter repeatedly with zero LUNs;
- add/delete fixed and removable LUNs at boundary sizes;
- invalid, truncated, oversized, old-version, and randomized control payloads;
- format NTFS/exFAT, fill the volume, hash data, flush, reread, and compare;
- parallel random reads/writes with forced add/delete races rejected safely;
- handle-held delete, surprise removal, shutdown, restart, sleep/resume, and VM
  checkpoint restore;
- low-memory and allocation-failure injection in its own VM pass;
- repeated start/stop and reinstall/upgrade of the same package.

Disable Verifier when the pass is complete:

```powershell
verifier /reset
shutdown /r /t 0
```

If the guest enters a boot loop, revert the checkpoint or use recovery mode to
reset Verifier. Preserve the memory dump first when possible.

## Soak acceptance gate

Before calling a milestone stable:

- 1,000 successful add/write/hash/delete cycles split across fixed/removable;
- 24 hours of concurrent random I/O with periodic flushes;
- 100 install/start/stop/uninstall cycles;
- clean runs on each supported Windows build and CPU architecture actually
  claimed by the release;
- zero bug checks, Verifier violations, stuck SRBs, pool leaks, data mismatches,
  unexpected event-log errors, or orphaned disk/volume devices.

Any bug check is a release blocker. Save the exact binary, symbols, verifier
settings, random seed, test log, and dump. Analyze with `!analyze -v` and
`!verifier 3 WispDisk.sys`, fix the root cause, and rerun the complete relevant
pass rather than only the crashing iteration.
