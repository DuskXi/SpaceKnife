//! sk-core: the SpaceKnife backend's domain logic (ported from packages/backend/src/spaceknife).
//! Each module is owned by one porting task; add only your own `pub mod` lines.
pub mod i18n;
pub mod paths;
pub mod fmt;
pub mod privacy;
pub mod log;
pub mod memory;
pub mod settings;
pub mod update;
pub mod classes;
pub mod volume;
pub mod media;
pub mod disks;
pub mod io;
pub mod helper;
pub mod scan;
pub mod monitor;
pub mod filter;
pub mod layout;
pub mod view;
pub mod diff;
pub mod ops;
pub mod export;
pub mod palette;
pub mod insights;
pub mod pathclass;
pub mod events;
