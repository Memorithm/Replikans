#![forbid(unsafe_code)]

#[path = "lib.rs"]
mod cycle;
mod replication_gate;

pub use cycle::*;
pub use replication_gate::{
    ArchiveReplicationError, assess_cycle_replication, consider_replication,
    consider_replication_from_archive, timeline_samples,
};
