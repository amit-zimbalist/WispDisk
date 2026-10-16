# C++ driver guidelines

These guidelines apply to `driver/` in addition to the shared root
`AGENTS.md`.

## Files and includes

- Pair each implementation `.cpp` file with a corresponding `.h` file
  describing that component's interface.
- Keep declarations close to their component. Avoid accumulating unrelated
  declarations in a shared internal header.
- Keep implementation details out of public headers when practical.
- Use configured include roots. Do not use `../` in source `#include`
  directives.
- Use includes such as `"driver.h"` and `"support/scoped_spin_lock.h"`.
  Maintain shared include paths and compiler settings in
  `driver/WispDisk.Common.props`.
- Update the Visual Studio project and filters when adding, moving,
  or splitting driver files.

## Implementation style

- Use modern C++ features supported by the project's C++20 toolchain
  and kernel environment.
- Prefer direct initialization and brace initialization. When values are
  known at construction, initialize the object there instead of following
  zero initialization with a long sequence of member assignments.
- Use designated initializers where they improve clarity and are supported
  by the type. Keep initialization order and required zero values correct.
- Use `const`, `noexcept`, and `[[nodiscard]]` where they express a useful
  contract. Make ownership-related copy and move behavior explicit.
- Keep driver callbacks focused on boundary validation and delegation.
  Put substantial behavior in the component responsible for it.
- Preserve callback role declarations and SAL annotations.

### Direct initialization example

The following fictional type illustrates initializing known fields together.
Keep designated initializers in member declaration order.

```cpp
struct BufferOptions {
    ULONG capacityBytes{};
    ULONG alignmentBytes{};
    bool zeroInitialize{};
};

const BufferOptions options{
    .capacityBytes = 4096,
    .alignmentBytes = 64,
    .zeroInitialize = true,
};
```

### RAII example

This fictional guard owns a scoped lock acquisition, not the underlying lock.
The lock must already be initialized and outlive the guard. Callers must meet
the Windows API's IRQL requirements.

```cpp
class ScopedQueueLock final {
public:
    explicit ScopedQueueLock(KSPIN_LOCK& lock) noexcept
        : lock_{lock} {
        KeAcquireSpinLock(&lock_, &previousIrql_);
    }

    ~ScopedQueueLock() noexcept {
        KeReleaseSpinLock(&lock_, previousIrql_);
    }

    ScopedQueueLock(const ScopedQueueLock&) = delete;
    ScopedQueueLock& operator=(const ScopedQueueLock&) = delete;
    ScopedQueueLock(ScopedQueueLock&&) = delete;
    ScopedQueueLock& operator=(ScopedQueueLock&&) = delete;

private:
    KSPIN_LOCK& lock_;
    KIRQL previousIrql_{};
};
```

Keep the acquisition's lifetime narrow with a named helper:

```cpp
void IncrementPending(KSPIN_LOCK& lock, ULONG& pending) noexcept {
    ScopedQueueLock guard{lock};
    ++pending;
} // Automatically releases the lock and restores the previous IRQL.
```

The same lock must protect all concurrent accesses to `pending`. Keep work
inside the helper bounded and nonblocking.

Use RAII when an object owns a resource or scoped obligation. Prevent copies
that would duplicate cleanup. A borrowed view does not own the underlying
resource and must not tear it down when destroyed.

## Kernel constraints and diagnostics

- Keep modern C++ choices compatible with kernel execution requirements.
  Do not introduce exceptions, RTTI, global constructors,
  standard-library allocation, or floating point into kernel code.
- Keep lock ownership and protected invariants clear. Avoid holding spin
  locks across allocation, waiting, I/O draining, request completion,
  or bus notifications.
- Add useful diagnostics at important lifecycle, management, and failure
  points using the existing diagnostic helpers, `KdPrint`, or `DbgPrint`.
- Keep diagnostics bounded and low volume. Avoid per-I/O logging in hot
  paths.

## Verification

- Keep C++ builds warning-clean and preserve WDK analysis annotations.
- Validate affected driver changes for x64 and ARM64.
- Follow `docs/testing.md` for runtime driver verification.
- Follow the root policy on excluding test harnesses and results from commits.
