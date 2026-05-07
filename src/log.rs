// Lightweight structured logging for mempalace.
// Uses stderr so it doesn't interfere with stdio MCP protocol on stdout.

/// Check if debug-level logging is enabled via RUST_LOG env var.
pub(crate) fn debug_enabled() -> bool {
    matches!(std::env::var("RUST_LOG").as_deref(), Ok("debug" | "trace"))
}

/// Log a message at the given level. Level: "info", "warn", "error", "debug".
/// Info, warn, and error always print. Debug only prints when RUST_LOG=debug (or trace).
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {
        {
            let _level = $level;
            if _level != "debug" || $crate::log::debug_enabled() {
                eprintln!("[{}] {}", _level, format!($($arg)*));
            }
        }
    };
}
pub(crate) use log;
