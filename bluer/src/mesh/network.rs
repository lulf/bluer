//! Implement Network interface

use crate::{Error, ErrorKind, InternalErrorKind};
use crate::{Result, SessionInner};
use std::sync::Arc;

use dbus::{
    nonblock::{Proxy, SyncConnection},
    Path,
};

use crate::mesh::{all_dbus_objects, SERVICE_NAME, TIMEOUT};

pub(crate) const INTERFACE: &str = "org.bluez.mesh.Network1";
pub(crate) const PATH: &str = "/org/bluez/mesh";

/// Interface to a Bluetooth adapter.
#[cfg_attr(docsrs, doc(cfg(feature = "bluetoothd")))]
#[derive(Clone)]
pub struct Network {
    inner: Arc<SessionInner>,
}

impl Network {
    pub(crate) fn new(inner: Arc<SessionInner>) -> Result<Self> {
        Ok(Self { inner })
    }

    fn proxy(&self) -> Proxy<'_, &SyncConnection> {
        Proxy::new(SERVICE_NAME, PATH, TIMEOUT, &*self.inner.connection)
    }

    /// Temprorary debug method to print the state of mesh
    pub async fn print_dbus_objects(&self) -> Result<()> {
        for (path, interfaces) in all_dbus_objects(&*self.inner.connection).await? {
            println!("{}", path);
            for (interface, _props) in interfaces {
                println!("    - interface {}", interface);
            }
        }
        Ok(())
    }

    /// Attach to mesh network
    pub async fn attach(&self, path: &str, token: &str) -> Result<()> {
        let token_int = u64::from_str_radix(token, 16)
            .map_err(|_| Error::new(ErrorKind::Internal(InternalErrorKind::InvalidValue)))?;

        let path_value =
            Path::new(path).map_err(|_| Error::new(ErrorKind::Internal(InternalErrorKind::InvalidValue)))?;

        self.call_method("Attach", (path_value, token_int)).await
    }

    /// Cancel provisioning request
    pub async fn cancel(&self) -> Result<()> {
        self.call_method("Cancel", ()).await
    }

    /// Leave mesh network
    pub async fn leave(&self, token: &str) -> Result<()> {
        let token_int = u64::from_str_radix(token, 16)
            .map_err(|_| Error::new(ErrorKind::Internal(InternalErrorKind::InvalidValue)))?;

        self.call_method("Leave", (token_int, )).await
    }

    dbus_interface!();
    dbus_default_interface!(INTERFACE);
}
