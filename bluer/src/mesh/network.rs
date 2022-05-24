//! Implement Network bluetooth mesh interface

use crate::{Error, ErrorKind, InternalErrorKind};
use crate::{Result, SessionInner};
use std::sync::Arc;

use dbus::{
    nonblock::{Proxy, SyncConnection},
    Path,
};

use crate::mesh::{all_dbus_objects, SERVICE_NAME, PATH, TIMEOUT, application::{Application, RegisteredApplication}};
use crate::mesh::application::ApplicationHandle;
use uuid::Uuid;
use crate::mesh::ElementConfig;

pub(crate) const INTERFACE: &str = "org.bluez.mesh.Network1";

/// Interface to a Bluetooth mesh network.
#[derive(Clone)]
pub struct Network {
    inner: Arc<SessionInner>,
}

impl Network {
    pub(crate) async fn new(inner: Arc<SessionInner>) -> Result<Self> {
        Ok(Self {
            inner,
        })
    }

    fn proxy(&self) -> Proxy<'_, &SyncConnection> {
        Proxy::new(SERVICE_NAME, PATH, TIMEOUT, &*self.inner.connection)
    }

    /// Create mesh application
    pub async fn application(&self, app: Application) -> Result<ApplicationHandle> {

        let reg = RegisteredApplication::new(self.inner.clone(), app);

        reg.register(self.inner.clone()).await
    }

    /// Temprorary debug method to print the state of mesh
    pub async fn print_dbus_objects(&self) -> Result<()> {

        // let proxy = Proxy::new("org.bluez.mesh", "/", TIMEOUT, &*self.inner.connection);
        // let (x,): (HashMap<dbus::Path<'static>, HashMap<String, PropMap>>,) = proxy.method_call("org.freedesktop.DBus.ObjectManager", "GetManagedObjects", ()).await.unwrap();
        // println!("om {:?}", x);

        for (path, interfaces) in all_dbus_objects(&*self.inner.connection).await? {
            println!("{}", path);
            for (interface, _props) in interfaces {
                println!("    - interface {}", interface);
            }
        }
        Ok(())
    }

    /// Join mesh network
    pub async fn join(&self, path: &str, uuid: Uuid) -> Result<()> {
        let path_value =
            Path::new(path).map_err(|_| Error::new(ErrorKind::Internal(InternalErrorKind::InvalidValue)))?;

        self.call_method("Join", (path_value, uuid.as_bytes().to_vec())).await
    }

    /// Attach to mesh network
    pub async fn attach(&self, path: &str, token: &str) -> Result<()> {
        let token_int = u64::from_str_radix(token, 16)
            .map_err(|_| Error::new(ErrorKind::Internal(InternalErrorKind::InvalidValue)))?;

        let path_value =
            Path::new(path).map_err(|_| Error::new(ErrorKind::Internal(InternalErrorKind::InvalidValue)))?;


        let (node, config): (Path, Vec<(u8, Vec<(u16, ElementConfig)>)>)
            = self.call_method("Attach", (path_value, token_int)).await?;

        log::info!("Attached app to {:?} with elements config {:?}", node, config);


        // TODO configure elements and get node object
        Ok(())
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
