//! `island-socket`: what a Vessel's agent may ask Island (docs/slice-2.md,
//! "The island socket"). HTTP over a Unix socket placed in the Vessels'
//! root filesystem; the caller is the connection's peer uid, looked up in
//! Taso's Warden config. It answers who the caller is and passes model
//! calls to the configured OpenAI-compatible providers, within each
//! Vessel's daily budget, and writes what it was asked to the Island log
//! as batched `access` entries. It never writes anything else, and never
//! on a caller's behalf.

pub mod access;
pub mod callers;
pub mod config;
pub mod server;
pub mod upstream;
pub mod usage;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Everything `serve` needs, read and checked before the socket opens.
pub fn shared(config: config::Config) -> Result<Arc<server::Shared>, String> {
    let callers = callers::Callers::load(&config.warden_config)?;
    let modified = std::fs::metadata(&config.warden_config)
        .and_then(|meta| meta.modified())
        .ok();
    std::fs::create_dir_all(&config.state_dir)
        .map_err(|error| format!("{}: {error}", config.state_dir.display()))?;
    let usage = usage::Usage::load(&config.state_dir, usage::today())?;
    let mut keys = BTreeMap::new();
    for (name, provider) in &config.provider {
        if let Some(path) = &provider.key_file {
            let key = std::fs::read_to_string(path)
                .map_err(|error| format!("provider {name}: {}: {error}", path.display()))?;
            keys.insert(name.clone(), key.trim().to_owned());
        }
    }
    let client = upstream::Client::new()?;
    Ok(Arc::new(server::Shared {
        warden_config: config.warden_config.clone(),
        config,
        keys,
        callers: Mutex::new((callers, modified)),
        usage: Mutex::new(usage),
        batches: Arc::default(),
        client,
    }))
}

/// Open the socket: any stale file goes first, and anyone may connect
/// (the peer uid decides what they get).
pub fn bind(path: &Path) -> Result<tokio::net::UnixListener, String> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", path.display())),
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(listener)
}
