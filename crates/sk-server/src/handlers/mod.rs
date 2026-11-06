//! Method handlers, one module per area (Python `server/*_handlers.py`).

pub mod classes;
pub mod compare;
pub mod export;
pub mod filter;
pub mod machine;
pub mod maintenance;
pub mod ops;
pub mod palette;
pub mod scan;
pub mod settings;
pub mod system;
pub mod update;
pub mod view;

use crate::rpc::Router;
use crate::topics::TopicRegistry;

/// Register every method and topic.
pub fn install(r: &mut Router, t: &mut TopicRegistry) {
    system::install(r, t);
    scan::install(r, t);
    filter::install(r, t);
    view::install(r, t);
    settings::install(r, t);
    palette::install(r, t);
    compare::install(r, t);
    classes::install(r, t);
    ops::install(r, t);
    export::install(r, t);
    maintenance::install(r, t);
    machine::install(r, t);
    update::install(r, t);
}
