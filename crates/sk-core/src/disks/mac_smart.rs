//! macOS: each NVMe disk's SMART / Health log (Python `disks/mac_smart.py`).
//!
//! TODO(system): the Python version reads it through IOKit's NVMe SMART user client
//! (`IOCreatePlugInInterfaceForService` → `QueryInterface` → `SMARTReadData`, COM vtables of
//! `NVMeSMARTLibExternal.h`). That FFI is not ported yet: it cannot be compiled or tried here, and
//! a wrong vtable offset would crash the backend. Until then macOS disks have no NVMe health
//! record (the page shows `health_note = unavailable`).

use std::collections::BTreeMap;

/// Whole-disk BSD name → its 512-byte NVMe SMART / Health log; empty for now.
pub fn nvme_smart_logs() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::new()
}
