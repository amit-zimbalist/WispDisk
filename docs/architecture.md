# Architecture

## Components

```text
wispdisk.exe (Rust, elevated only when needed)
    |
    | SetupAPI / Configuration Manager: install signed root-enumerated package
    | IOCTL_SCSI_MINIPORT: versioned management messages
    v
WispDisk.sys (C++ StorPort virtual miniport)
    |
    | reports virtual SCSI LUNs and completes SRBs
    v
storport.sys -> disk.sys -> partmgr.sys -> volume/file-system stack
```

The CLI owns package extraction, installation, device discovery, disk
initialization, formatting, and drive-letter assignment. The miniport owns LUN
lifetime and block I/O. Drive letters are mount-manager state and must not be
treated as kernel disk identity.

## Add flow

1. Parse and validate `/add`, media kind, drive letter, and byte size without
   elevation.
2. Verify the requested letter is free.
3. Require the process to be elevated for the privileged phase; Windows API
   errors retain their Win32 error codes when it is not.
4. Extract the embedded `.sys`, `.inf`, and `.cat` into a versioned directory
   below `%ProgramData%\WispDisk\Driver`.
5. Stage/install the root-enumerated `ROOT\WISPDISK` adapter with SetupAPI and
   Configuration Manager. A StorPort miniport is PnP software; loading only the
   `.sys` through the Service Control Manager is not sufficient packaging.
6. Discover the installed `\\.\ScsiN:` adapter by protocol signature and issue
   the protocol version/capability query before any mutation.
7. Send a create request. The driver validates all fields, allocates a unique
   device ID, publishes a LUN, and notifies StorPort of the state change.
8. Wait for `disk.sys` and match the returned path/target/LUN plus the adapter's
   SCSI port through `IOCTL_SCSI_GET_ADDRESS`. Never trust enumeration order or
   a `PhysicalDriveN` number on its own.
9. Initialize the disk, create one partition, format it, and assign the
   requested mount point. These are separate operations with separate rollback.

Immediately before formatting, revalidate the physical disk's SCSI address.
PowerShell separately rejects a disk that is not RAW, has become boot/system,
or no longer has the exact requested size. If a later add step fails, the CLI
requests LUN deletion. Never delete or initialize a disk selected only by its
current `PhysicalDriveN` number.

## Delete flow

1. Resolve the drive letter to a single-disk volume, query its SCSI address, and
   match that address uniquely against the driver's live-LUN list to recover the
   stable WispDisk device ID.
2. Lock and dismount the volume. Refuse deletion when handles remain unless a
   future explicit force option is designed.
3. Remove the mount point.
4. Mark the LUN as stopping so new I/O fails predictably, drain outstanding
   synchronous SRBs, notify StorPort, and wait for PnP removal.
5. Free backing pages only after the active-request count reaches zero.
6. Confirm that the exact SCSI location is absent before reporting success.

Deletion by drive letter fails closed once the mount is absent. The protocol
returns `STATUS_NO_SUCH_DEVICE` for a stale device ID and never redirects it to
another LUN.

## Storage data path

The current functional version exposes one target with up to 16 LUNs and
supports:

- REPORT LUNS;
- TEST UNIT READY and REQUEST SENSE;
- INQUIRY plus serial/device-ID VPD pages;
- READ CAPACITY (10/16);
- READ and WRITE (10/16);
- SYNCHRONIZE CACHE;
- MODE SENSE needed by the class/format stack;
- illegal-command, invalid-CDB, and out-of-range sense data.

The fixed/removable choice belongs in the INQUIRY response. It is immutable for
the life of a LUN.

Each disk is capped at 256 MiB, total live backing is capped at 512 MiB, and RAM
is allocated/zeroed in a StorPort work item at PASSIVE_LEVEL. `HwStartIo`, which
StorPort may call at IRQL up to DISPATCH_LEVEL, only performs bounded validation
and synchronous memory copies. A sparse or user-mode-backed design is a later,
separate decision.

## Concurrency invariants

- Every LUN has `Creating -> Online -> Stopping -> Removed` states.
- The adapter table holds a reference for every published LUN.
- Each accepted SRB holds a LUN reference until exactly one completion.
- Removal prevents new references, cancels queued work, drains active work, and
  only then releases storage.
- Buffer length, CDB length, LBA arithmetic, offset arithmetic, and protocol
  structure size are checked before access.
- Unknown protocol versions, flags, operations, and nonzero reserved fields are
  rejected.
- No C++ exceptions, RTTI, global constructors, standard-library allocation, or
  floating point exists in kernel code.

## Protocol

Management requests use `IOCTL_SCSI_MINIPORT` with `SRB_IO_CONTROL.Signature`
set to `WISPDK01`. The shared payload starts with a fixed-size header containing
the protocol version, operation, flags, and correlation ID. Create returns a
stable device ID plus SCSI address. Delete takes that ID, never a drive letter.

The Rust definitions in `cli/src/protocol.rs` mirror
`shared/wispdisk_protocol.h`; compile-time size checks protect the ABI on both
sides. Future changes append fields and increase `StructSize`; they do not
silently reinterpret version 1 layouts.

## Milestones

1. Skeleton: CLI grammar/tests, package embedding, INF, miniport registration,
   query-version request. **Complete.**
2. Bounded RAM backing, SCSI read/write/discovery, dynamic add/delete, and CLI
   partition/format/mount orchestration. **Implemented; awaiting VM tests.**
3. VM load/unload with WinDbg, test signing, and baseline Driver Verifier clean.
4. Invalid-control and boundary-size test automation.
5. Concurrent I/O, add/delete/PnP races, sleep/restart, and low-memory testing.
6. Expanded compatibility and soak matrix, then removal of alpha status.
