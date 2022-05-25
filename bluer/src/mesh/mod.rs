//! Bluetooth Mesh module

pub mod application;
pub mod network;
mod types;
pub use types::*;

use crate::{Result, SessionInner};
use dbus::{
    arg::{PropMap, RefArg, Variant},
    nonblock::{stdintf::org_freedesktop_dbus::ObjectManager, Proxy, SyncConnection},
    Path,
};
use dbus_crossroads::{Crossroads, IfaceBuilder, IfaceToken};
use std::{collections::HashMap, sync::Arc, time::Duration};

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

type ElementConfig = HashMap<String, Variant<Box<dyn RefArg + 'static>>>;

/// Interface to a Bluetooth mesh element interface.
#[derive(Debug)]
pub struct Element {
    /// Element models
    pub models: Vec<Box<dyn Model>>,
}

/// An element exposed over D-Bus to bluez.
pub struct RegisteredElement {
    inner: Arc<SessionInner>,
    element: Element,
    index: u8,
}

impl RegisteredElement {
    pub(crate) fn new(inner: Arc<SessionInner>, element: Element, index: u8) -> Self {
        Self { inner, element, index }
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
                let mut mt: Vec<(u16, ElementConfig)> = vec![];
                // TODO rewrite
                for model in &reg.element.models {
                    if let ModelIdentifier::SIG(id) = model.identifier() {
                        // TODO what about opts?
                        mt.push((id, HashMap::new()));
                    }
                }
                Some(mt)
            });
            cr_property!(ib, "VendorModels", reg => {
                let mut mt: Vec<(u16, u16, ElementConfig)> = vec![];
                for model in &reg.element.models {
                    if let ModelIdentifier::Vendor(vid, id) = model.identifier() {
                        mt.push((vid.0, id, HashMap::new()));
                    }
                }
                Some(mt)
            });
        })
    }
}
