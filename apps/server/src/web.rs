//! The web front end: the HTTP(S) listeners ([`http`]), the WebSocket that
//! streams the snapshot down and takes the portal's messages up ([`ws`]),
//! the mapping of those messages onto the vehicle's typed operations
//! ([`portal`]) and the self-signed certificate the browser's microphone
//! needs ([`tls`]).

pub mod http;
pub mod portal;
pub mod tls;
pub mod ws;

use std::path::PathBuf;
use std::sync::Arc;

use dji_vehicle::Vehicle;

use crate::video::Feeds;

/// What every connection handler shares.
pub struct Context {
    pub web_root: PathBuf,
    pub feeds: Arc<Feeds>,
    pub vehicle: Arc<Vehicle>,
}
