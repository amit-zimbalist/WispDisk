# Rust CLI guidelines

These guidelines apply to `cli/` in addition to the shared root
`AGENTS.md`.

## Modules and APIs

- Split modules by responsibility and give substantial resource-owning
  types their own modules.
- Keep orchestration readable by delegating installation, discovery,
  notifications, storage provisioning, and volume operations to focused
  components.
- Group related operations in `impl` blocks when they operate on the
  same resource or state. `DriverPackage` and `CreatedDevice` are examples.
- Use the narrowest visibility needed, including private items and
  `pub(super)` for implementation helpers.
- Prefer idiomatic Rust types and standard traits over repeated manual
  field manipulation.

## Validation and conversion

- Put response validation on the response type or in a conversion that
  represents its meaning.
- Use `TryFrom` when conversion requires validation and can fail.
  Use `From` only when conversion is infallible.
- Keep request-specific identity checks in the operation or recovery layer
  that knows the expected identity.
- Preserve recovery information when validation or conversion fails.

### Fallible conversion example

This fictional type accepts only valid batch sizes. Validation belongs in
`TryFrom` because conversion can fail.

```rust
struct BatchSize(u32);

impl TryFrom<u32> for BatchSize {
    type Error = &'static str;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value == 0 || value > 128 {
            return Err("batch size must be between 1 and 128");
        }

        Ok(Self(value))
    }
}
```

The static error string keeps this small example self-contained. Use typed
errors or `anyhow` with context as appropriate in application code.

## Error handling

- Use `anyhow::Result` and `Context` for CLI orchestration to keep error
  handling concise and informative.
- Preserve typed errors where callers need to distinguish conditions.
- Retain underlying I/O, WMI, and Win32 errors rather than flattening
  them into strings.
- Add context describing the operation that failed.
- If rollback also fails, preserve both the original failure and the
  recovery failure.

## Platform integration and diagnostics

- Use Rust and native Windows APIs directly for runtime operations.
  Use the WMI crate for WMI operations instead of spawning PowerShell
  or other processes.
- Encapsulate unsafe FFI and resource cleanup in focused components.
  Make the safety assumptions clear at unsafe boundaries.
- Avoid threading optional diagnostic state through every function.
  Use the existing opt-in process-wide timing facility for CLI timings.
- Keep disabled diagnostics inexpensive. Diagnostic failures must not
  interrupt normal operations or recovery.

## Verification

- Format Rust with `cargo fmt`.
- Run relevant Rust checks and tests for the affected behavior, including
  Clippy and the workspace tests where applicable.
- Follow the root policy on excluding test harnesses and results from commits.
