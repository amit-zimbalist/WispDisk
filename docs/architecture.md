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

### Rust module boundaries

`cli/src/driver.rs` is the platform-independent entry point. The Windows
implementation lives in `cli/src/driver/windows/`:

- `mod.rs`: add/delete orchestration and shared wait durations;
- `adapter.rs`: lazy adapter discovery and validated miniport protocol calls;
- `install.rs`: embedded package extraction and root-device installation;
- `recovery.rs`: retains the primary error and a failed rollback/recovery error;
- `disk.rs`: disk-interface discovery and SCSI identity checks;
- `disk_interface_notification.rs`: arrival subscription, callback, and cleanup;
- `storage.rs`: WMI disk/partition/volume models and provisioning;
- `volume.rs`: drive-letter state, volume identity, locks, and mount rollback;
- `win32.rs`: shared handle ownership, device opening, encoding, and errors.

`cli/src/logging.rs` owns the opt-in logging lifetime. Its `subscriber` module
installs a process-wide `tracing` subscriber, formats UTC system-clock timestamps,
and owns the `tracing-appender` stdout writer guard. Its Windows `driver_events`
module owns a uniquely named `ferrisetw::UserTrace` session and a joinable ordinary
thread; a custom kernel-mode TraceLogging provider uses a regular provider
session, not the system kernel logger. No Tokio runtime is used.

The ETW callback decodes WispDisk's fixed event fields and forwards the original
event timestamp and severity into the same subscriber. Windows converts the
session's QPC timestamps to FILETIME before callback delivery. A central conversion
uses the intact raw timestamp because ferrisetw 1.2's `timestamp()` duplicates the
high FILETIME word and loses subsecond precision. Callback views
do not escape; forwarded fields are copied/formatted synchronously. The formatter
uses original ETW time for driver records and current system time for CLI events.
Arrival order across sources need not match event-time order.

Shutdown stops the owned ETW session through `ferrisetw::stop_trace_by_name`, lets `ProcessTrace`
drain, joins its thread, and finally drops the stdout writer guard. This ordering
avoids `ferrisetw`'s default close-before-stop behavior, which suppresses pending
callbacks. `Logging::drop` performs cleanup and reports shutdown failures on
stderr. Requested logging must initialize successfully before command execution;
initialization failures print an error to stderr and exit. Later diagnostic
failures never replace command/recovery failures. Bounded
queues can drop events; stdout queue loss counts are reported.

`driver/src/logging.h/.cpp` owns a private TraceLogging provider with GUID
`8b6cae34-6a65-4cf1-a55b-0c9a5c224896`. StorPort's `HwInitializeTracing` and
`HwCleanupTracing` callbacks manage registration independently of adapter views.
Failed DriverEntry also performs idempotent cleanup. Lifecycle, management,
allocation, and failure events are available in Release and Debug builds. The
existing Debug circular trace and fault-injection controls remain available.
There is no per-block-I/O logging. Calls above DISPATCH_LEVEL skip ETW, including
adapter-control callbacks when invoked at DIRQL. Provider state and payloads must
remain resident, and logging never waits on user-mode output or performs file I/O.

Implementation helpers are private or restricted to their parent module. The
notification module owns the event and unregisters its callback before closing
the event handle. Adapter discovery has one lazy scan: creation stops at the
first match, while deletion visits all adapters to verify unique ownership.

`DriverPackage` owns the extracted INF path and its installation operations.
`CreatedDevice` owns a newly created root-device handle and exposes explicit
rollback removal; dropping it never removes a successfully installed device.

CLI operations use `anyhow::Result` with context rather than stringifying
underlying I/O and WMI errors. Platform preconditions retain typed `DriverError`
variants, Win32 error codes remain inspectable, and the CLI prints the complete
context chain. If recovery also fails, its error is retained alongside the
primary failure rather than replacing or flattening it.

Protocol responses expose validation methods for their specific meaning.
`TryFrom<OperationResponse>` constructs `CreatedDisk` or a reserved
`OperationHandle` only after validating the corresponding response. Request
identity matching stays in recovery, and failed create conversion retains the
original unresolved-operation handle.

### Miniport adapter context

The adapter components are non-owning views over StorPort's plain extension
storage. `AdapterContext` coordinates initialization, SRB dispatch, stop/restart,
and explicit teardown. Its `Management()` and `Disks()` accessors return
lightweight views backed by the existing extension storage.

| Component | Public API and responsibility |
| --- | --- |
| `adapter_context.h/.cpp` | `Initialize`, `SetStopping`, `StartIo`, `ReleaseResources`: lifecycle and dispatch. |
| `management/management_controller.h/.cpp` | `Initialize`, `Shutdown`, `ReserveOperation`, `QueryOperation`, `SubmitCreate`, `SubmitDelete`, `SnapshotState`: management lifecycle, admission, replay, worker handoff, retained outcomes, and completion. `ProcessWorkItem` is the StorPort worker entry point. |
| `storage/lun_store.h/.cpp` | `AcquireIo`, `SnapshotOnlineDisks`, `SnapshotOnlineLuns`: leased I/O and online inventory. Mutation and draining primitives are private to management coordination. |
| `storage/lun_io_lease.h/.cpp` | `operator bool`, `view`: an active I/O reference released by the destructor. The view is usable only while the lease is alive. |
| `management/diagnostics.h/.cpp` | Namespace functions for bounded tracing and Debug-only diagnostic controls. |

Protocol handlers validate/decode wire requests before calling the controller.
The borrowed `ControlRequest` view is constructed through `FromSrb`, which
returns an empty view for a null SRB or invalid control envelope. Its typed
`ReadRequest` and `WriteResponse` methods use the shared `WireStructure` concept
to require trivially copyable, standard-layout types and derive their wire sizes.
Request decoding preserves the common header on validation errors; response
writing checks capacity before copying. `WriteError` preserves the original
status when a buffer cannot fit an error header. `PrepareCompletion` updates
response metadata; SRB completion and ownership transfer remain explicit.
Operation-specific validation and response-capacity checks before mutations stay
in the handlers, along with the short-buffer fallback for simple error responses.
Reservation and query results are copied values; table pointers and locking
views do not escape through the public API. The controller admits management
work, binds immutable replay parameters, and publishes Pending under one
`LunLock` acquisition. Its inventory snapshot reads both LUNs and pending
operations under that same lock. Every `...Locked` helper accepts a live guard
and asserts that it protects the correct adapter lock in Debug builds. The
extension storage documents which fields use that lock and which lifecycle
updates run outside it; blocking worker and teardown APIs declare PASSIVE_LEVEL
requirements through SAL. Required internal output arguments use references.

Pool allocation, I/O draining, SRB completion, and bus notifications run outside
the spin lock. After backing-store release, the controller finalizes LUN removal
and marks its create record Deleted together under the lock. Reserved create
results are published with the LUN's creating-operation ID under the lock.
Stop/restart callbacks use atomic flag updates. The controller initializes its
management state and worker through `Initialize`. Its `Shutdown` closes admission
under the lock, waits for the final worker access, drains/releases LUNs, and frees
the worker. `AdapterContext` delegates to these lifecycle methods and invalidates
the adapter signature after shutdown returns; it needs no friendship with the
controller. Waiting and worker-release helpers remain private.

Driver C++ formatting is defined by the repository's `.clang-format`, including
the layout of WDK annotations. Diagnostic control codes and injection modes have
shared names in `management/diagnostics.h`; their numeric wire values stay fixed.
`ControlRequest::ioControl` names the borrowed SRB control header.

## Add flow

1. Parse and validate `/add`, media kind, drive letter, byte size, and selected
   filesystem without elevation. NTFS is the default.
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
8. Register for `GUID_DEVINTERFACE_DISK` arrival, enumerate the present disk
   interfaces, and match the returned path/target/LUN plus the adapter's SCSI
   port through `IOCTL_SCSI_GET_ADDRESS`. Resolve the matching interface to its
   disk number with `IOCTL_STORAGE_GET_DEVICE_NUMBER`. Never trust enumeration
   order or a `PhysicalDriveN` number on its own.
9. Initialize the disk, create one filesystem-appropriate MBR partition with
   the requested drive letter, format it as FAT, FAT32, or NTFS, and verify the
   mount. These are separate operations with shared rollback. Consume the
   partition object returned by the create operation instead of waiting for a
   second provider-cache query.

Immediately before formatting, revalidate the physical disk's SCSI address.
The WMI provisioning layer rejects an unexpected partition style, boot/system
disks, or a disk without the exact requested size. If a later add step fails,
the CLI requests LUN deletion. Never delete or initialize a disk selected only
by its current `PhysicalDriveN` number.

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

### Recoverable management operations

`WISPDISK_CAP_OPERATION_RECOVERY` advertises additional operations without
changing the original v1 layouts. ReserveOperation allocates a bookkeeping
handle `(AdapterInstance, OperationId)` for create or delete; it does not
allocate a LUN. CreateReserved/DeleteReserved bind immutable parameters and
publish Pending under the same lock used for management admission. Replaying
an accepted handle returns its state and result, never another mutation.
QueryOperation returns Reserved, Pending, Succeeded, Failed, or Deleted. Deleted
means a successful create's allocation has since been removed and cannot be
resurrected by replay.

The fixed 128-record table never evicts pending mutations or successful creates
with live allocations. Empty slots are used first, then the oldest unused
reservation or terminal record is reclaimed. An unknown/expired handle is
rejected, including for mutation; operation IDs never wrap or become valid
again within an adapter instance. AdapterInstance changes on adapter creation.
These records are volatile and do not promise persistence across reboot or
checkpoint restore. Transport recovery must retain the original handle.

ListState exposes online and stopping LUNs, their creating operation IDs,
allocated bytes, and pending mutations. Stopping LUNs remain visible and counted
until backing memory has actually been freed. The original ListDisks remains
an online-only inventory for compatibility. A successful delete is published
only after backing-store release; Windows PnP disappearance is a separate CLI
completion condition.

Management SRBs are pending before work is queued. After successful queueing,
only the worker accesses the SRB. It finalizes the response, completes the SRB,
and notifies the bus using retained adapter context. Its idle signal follows all
completion and notification accesses and is protected by LunLock. Teardown
closes admission under that lock and waits until the worker can be freed.

The CLI queries the original reserved handle after a transport failure. Reserved
can be submitted again, Pending is polled, Succeeded returns the original disk,
and Failed is a confirmed rejection. Unknown outcomes retain recovery identity
and never cause another create. Initialization rollback uses the ordinary safe
deletion path, including volume locks, driver reconciliation, and PnP checks.

Deletion by adapter instance/device ID enumerates volume GUIDs and mount points,
matches them by physical disk and verified SCSI location, and locks every target
volume before dismount/removal. After rejection it restores mounts only when
inventory confirms that the same allocation remains online. An unresolved
deletion is reported for subsequent inventory-based recovery.

## Milestones

1. Skeleton: CLI grammar/tests, package embedding, INF, miniport registration,
   query-version request. **Complete.**
2. Bounded RAM backing, SCSI read/write/discovery, dynamic add/delete, and CLI
   partition/format/mount orchestration. **Implemented; awaiting VM tests.**
3. VM load/unload with WinDbg, test signing, and baseline Driver Verifier clean.
4. Invalid-control and boundary-size test automation.
5. Concurrent I/O, add/delete/PnP races, sleep/restart, and low-memory testing.
6. Expanded compatibility and soak matrix, then removal of alpha status.
