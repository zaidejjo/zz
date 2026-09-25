//! ZZ Plugin System
//!
//! Loads and validates `.zzi` plugin manifests, providing external function
//! signatures to the type checker without modifying `zz_lang` source.
//!
//! For the VM interpreter path, provides dlopen-based loading of plugin
//! `.so`/`.dylib` libraries.

pub mod loader;
pub mod manifest;

pub use loader::{
    load_c_plugin, load_plugin, LoadError, PluginLib, CURRENT_ABI_VERSION, CURRENT_C_ABI_VERSION,
};
pub use manifest::{load_manifest, ManifestError, ManifestMeta, PluginManifest};
