//! spaceknife-native: optional native acceleration for the SpaceKnife backend.
//!
//! Every function here has a pure-Python fallback in the backend; the backend imports this module
//! only when it is installed (`uv sync --group native`). Heavy work releases the GIL.

use pyo3::prelude::*;

#[cfg(feature = "dupes")]
mod dupes;
#[cfg(feature = "hardscan")]
mod hardscan;
#[cfg(feature = "mft")]
mod mft;
#[cfg(windows)]
mod winio;

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    #[cfg(feature = "mft")]
    mft::register(m)?;
    #[cfg(feature = "hardscan")]
    hardscan::register(m)?;
    #[cfg(feature = "dupes")]
    dupes::register(m)?;
    Ok(())
}
