//! Bluetooth Mesh module

pub mod network;
pub mod application;

use dbus::nonblock::stdintf::org_freedesktop_dbus::ObjectManager;
use dbus::{
    arg::{PropMap, RefArg, Variant},
    nonblock::{Proxy, SyncConnection},
    Path,
};
use std::collections::HashMap;
use std::time::Duration;
use std::sync::Arc;
use dbus_crossroads::{Crossroads, IfaceBuilder, IfaceToken};
use crate::{Result, SessionInner};

pub(crate) const SERVICE_NAME: &str = "org.bluez.mesh";
pub(crate) const PATH: &str = "/org/bluez/mesh";
pub(crate) const TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) const ELEMENT_INTERFACE: &str = "org.bluez.mesh.Element1";

/// Gets all D-Bus objects from the BlueZ service.
async fn all_dbus_objects(
    connection: &SyncConnection,
) -> Result<HashMap<Path<'static>, HashMap<String, PropMap>>> {
    let p = Proxy::new(SERVICE_NAME, "/", TIMEOUT, connection);
    Ok(p.get_managed_objects().await?)
}


/// Interface to a Bluetooth mesh element interface.
#[derive(Clone, Debug)]
pub struct Element {
    /// Element models
    pub models: Vec<Model>,
}

/// An element exposed over D-Bus to bluez.
#[derive(Clone)]
pub struct RegisteredElement {
    inner: Arc<SessionInner>,
    element: Element,
    index: u8,
}

impl RegisteredElement {
    pub(crate) fn new(inner: Arc<SessionInner>, element: Element, index: u8) -> Self {
        Self {
            inner,
            element,
            index,
        }
    }

    fn proxy(&self) -> Proxy<'_, &SyncConnection> {
        Proxy::new(SERVICE_NAME, PATH, TIMEOUT, &*self.inner.connection)
    }

    dbus_interface!();
    dbus_default_interface!(ELEMENT_INTERFACE);

    pub(crate) fn register_interface(cr: &mut Crossroads) -> IfaceToken<Arc<Self>> {
        cr.register(ELEMENT_INTERFACE, |ib: &mut IfaceBuilder<Arc<Self>>| {
            cr_property!(ib, "Index", reg => {
                Some(reg.index)
            });
            cr_property!(ib, "Models", reg => {
                let mut mt: Vec<(u16, HashMap<String, Variant<Box<dyn RefArg  + 'static>>>)> = vec![];
                // TODO rewrite
                for model in &reg.element.models {
                    if model.vendor == 0xffff {
                        // TODO what about opts?
                        mt.push((model.id, HashMap::new()));
                    }
                }
                Some(mt)
            });
            cr_property!(ib, "VendorModels", reg => {
                let mut mt: Vec<(u16, u16, HashMap<String, Variant<Box<dyn RefArg  + 'static>>>)> = vec![];
                for model in &reg.element.models {
                    if model.vendor != 0xffff {
                        mt.push((model.vendor, model.id, HashMap::new()));
                    }
                }
                Some(mt)
            });
        })
    }
}

/// Interface to a Bluetooth mesh model interface.
#[derive(Clone, Debug)]
pub struct Model {
    /// Model id
    pub id: u16,
    /// Model vendor
    pub vendor: u16,
}