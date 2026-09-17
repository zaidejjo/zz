//! ZZ Plugin System
//!
//! Loads and validates `.zzi` plugin manifests, providing external function
//! signatures to the type checker without modifying `zz_lang` source.

pub mod manifest;

pub use manifest::{load_manifest, ManifestError, ManifestMeta, PluginManifest};
