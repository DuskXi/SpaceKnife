//! Disk-load sampling and the per-disk I/O governor (Python `spaceknife.io`, docs/native.md §4).
//!
//! Typical wiring:
//!
//! ```ignore
//! let registry = IoRegistry::new(RegistryOptions { max_workers: pool_size, ..Default::default() });
//! registry.start();                                   // at backend start: pre-scan baseline
//! let gov = registry.governor_for_path(root);         // shared by every scan on that disk
//! gov.set_level(Level::Quiet, None);
//! let permit = gov.permit();                          // blocks; dropped when the batch is done
//! let id = gov.subscribe(Arc::new(push_status));      // 2 Hz IoStatus pushes
//! ```

pub mod disks;
pub mod governor;
pub mod priority;
pub mod registry;
pub mod sampler;

pub use disks::{DiskId, DiskKind, DiskRef, disk_for_path, disk_ref_for_path, local_disks};
pub use governor::{GovernorCore, IoGovernor, IoStatus, Level, LoadPoint, Permit, level_limit};
pub use priority::{io_priority_is_low, no_cloud_downloads, set_low_io_priority, set_thread_low_io_priority};
pub use registry::{IoRegistry, RegistryOptions};
pub use sampler::{DiskSample, DiskSampler, default_sampler};
