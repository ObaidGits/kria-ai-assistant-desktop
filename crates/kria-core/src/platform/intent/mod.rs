//! OS-intent dispatch subsystem.
//!
//! Provides the typed `Capability` sandbox, scheme allow-list, contact resolution,
//! cross-platform `OsIntentBackend` trait, and `IntentDispatcher`.

pub mod capability;
pub mod dispatcher;
pub mod grammar;
pub mod linux;
pub mod macos;
pub mod resolution;
pub mod scheme;
pub mod windows;

// Re-export the most used types for convenience.
pub use capability::{CanonicalAppId, Capability, SafeArg, SandboxedPath};
pub use dispatcher::{DispatchError, DispatchResult, IntentDispatcher, OsIntentBackend};
pub use grammar::{capability_schema, validate_capability_json};
pub use resolution::{Candidate, ContactId, ContactResolver, MessagingApp, ResolutionError};
