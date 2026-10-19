# Shared code guidelines

These guidelines apply throughout WispDisk. Follow them when adding or
refactoring code. Keep unrelated changes outside the task. Language-specific
guidance is in `cli/AGENTS.md` for the Rust CLI and `driver/AGENTS.md` for
the C++ driver.

## Shared design principles

- Keep code modular, readable, and DRY. Give each module and type a clear
  responsibility and a small, understandable API.
- Use descriptive names for files, directories, types, and functions.
  Name components after their responsibility rather than a broad technique
  such as `kernel_raii`.
- Split large files by responsibility. A file approaching or exceeding
  500 lines is a reason to review its boundaries, not an automatic line limit.
- Give substantial components their own files. For example,
  `DiskInterfaceNotification` belongs in a dedicated module.
- Declare structs and classes at module/file or namespace scope, never inside
  functions or methods. This includes resource guards and helper types.
- Organize directories around coherent responsibilities. Avoid both
  catch-all modules and excessive fragmentation.
- Put operations on a type when they naturally use its state, identity,
  or owned resources. Keep stateless helpers as free functions.
- Use focused collaborating types when one class accumulates multiple
  responsibilities. Each type should expose a clear API and hide its
  internal bookkeeping.
- Keep multi-resource coordination separate from per-resource operations, with
  distinct responsibility-based names such as `DiskDeletion` and `Volume`.
- Prefer named helper functions over standalone brace blocks introduced
  only to control resource or lock lifetime. Preserve the narrow lifetime
  when extracting the helper.
- Follow existing naming and formatting conventions in the surrounding
  code. Keep comments focused on intent, invariants, ownership, and
  non-obvious constraints.

## Ownership and resource lifetime

- Use RAII in C++ and ownership with `Drop` in Rust for resources that
  have a well-defined owner and reliable cleanup operation.
- Keep resource wrappers small, focused, and reusable. Acquire and validate
  resources in constructors or fallible factories, expose operations as methods,
  and release owned resources in destructors or `Drop`, including on early returns.
- Enumeration types should own their native handle, buffers, and iteration state.
  Keep advancing, end-of-enumeration detection, and error handling inside the type;
  use standard iterator interfaces where appropriate.
- Use RAII for owned resources or scoped obligations. Keep their lifetimes
  narrow and prevent copies that would duplicate cleanup.
- Distinguish owning objects from borrowed views. A borrowed view must
  not release or tear down the resource it references.
- Destruction should follow the lifetime of the owned resource.
  A temporary callback wrapper must not tear down adapter state that
  outlives the callback.
- Keep fallible lifecycle operations and rollback explicit when callers
  need to inspect their result. Destructors and `Drop` must not hide an
  important failure or unexpectedly undo a successful operation.
- Keep raw handles, pointers, and synchronization details behind the
  appropriate component API whenever practical.
- Preserve behavior, protocol layout, ownership, and synchronization
  invariants during style refactoring.

## Test harnesses and results

- Do not stage or commit test harness code or scripts, test results,
  logs, dumps, or other generated testing artifacts.
- You may create and run temporary harnesses to verify changes.
  Keep them outside the repository when practical.
- Before committing, inspect the staged changes and exclude these files.
- This restriction does not prohibit running tests or maintaining ordinary
  unit tests alongside the production code.
