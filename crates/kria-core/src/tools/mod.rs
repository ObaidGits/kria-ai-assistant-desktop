pub mod registry;
pub mod system_info;
pub mod file_ops;
pub mod app_lifecycle;
pub mod shell;
pub mod internet;
pub mod knowledge;
pub mod system_config;
pub mod power;
pub mod process;
pub mod documents;
pub mod communication;
pub mod interaction;
pub mod disk;
pub mod scheduler;

pub use registry::{ToolRegistry, ToolDef, ToolHandler};
