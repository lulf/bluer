//! Bluetooth Mesh module

pub mod network;
pub mod application;

use crate::Result;
use dbus::nonblock::stdintf::org_freedesktop_dbus::ObjectManager;
use dbus::{
    arg::PropMap,
    nonblock::{Proxy, SyncConnection},
    Path,
};
use std::collections::HashMap;
use std::time::Duration;

pub(crate) const SERVICE_NAME: &str = "org.bluez.mesh";
pub(crate) const PATH: &str = "/org/bluez/mesh";
pub(crate) const TIMEOUT: Duration = Duration::from_secs(120);

/// Gets all D-Bus objects from the BlueZ service.
async fn all_dbus_objects(
    connection: &SyncConnection,
) -> Result<HashMap<Path<'static>, HashMap<String, PropMap>>> {
    let p = Proxy::new(SERVICE_NAME, "/", TIMEOUT, connection);
    Ok(p.get_managed_objects().await?)
}
