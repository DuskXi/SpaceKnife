//! The events the page listens to (app/lib/tauriHost.ts EVENTS).

/// The backend's state changed: payload shared/api.ts BackendState.
pub const BACKEND: &str = "sk://backend";
/// The window was maximized (true) or restored (false).
pub const MAXIMIZED: &str = "sk://maximized";
