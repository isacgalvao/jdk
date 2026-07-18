//! Exit-code contract shared by the CLI and the shim. Codes reserved for
//! features not yet built (3 no-local-version, 13 permission, 28 disk...) are
//! adopted as those features land.

/// Success; the shim otherwise propagates the child's exit code verbatim.
pub const OK: i32 = 0;
/// Unexpected failure (I/O, spawn error).
pub const FAILURE: i32 = 1;
/// User or configuration error: malformed selector, pin file or config.toml.
pub const CONFIG: i32 = 2;
/// The resolved version is not installed (also: no global JDK configured).
pub const NOT_INSTALLED: i32 = 4;
/// Network failure past the retry schedule (index and fallback unreachable).
pub const NETWORK: i32 = 20;
/// The requested tool does not exist in the resolved JDK ("command not found").
pub const TOOL_NOT_FOUND: i32 = 127;
