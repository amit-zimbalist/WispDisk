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
