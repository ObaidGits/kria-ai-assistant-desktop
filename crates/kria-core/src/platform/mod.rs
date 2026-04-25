pub mod app_registry;
pub mod contacts;
pub mod detect;
pub mod inbox;
pub mod intent;
pub mod os;
pub mod paths;
pub mod sandbox;
pub mod telegram;
pub mod vram;

pub use detect::*;
pub use paths::*;
pub use sandbox::install_seccomp_filter;
pub use vram::{
    build_profiler, GpuVendor, ImageTier, NullProfiler, RocmProfiler, VramBarrier, VramProfiler,
    VramSnapshot,
};
