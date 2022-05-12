//! Implement Application bluetooth mesh interface

use crate::{method_call, Result, SessionInner};
use std::sync::Arc;

use dbus::{
    nonblock::{Proxy, SyncConnection},
};
use dbus_crossroads::{Crossroads, IfaceBuilder, IfaceToken};

use crate::mesh::{SERVICE_NAME, PATH, TIMEOUT};

pub(crate) const INTERFACE: &str = "org.bluez.mesh.Application1";

/// Interface to a Bluetooth mesh network interface.
#[derive(Clone)]
pub struct Application {
    inner: Arc<SessionInner>,
    path: String,
}

impl Application {
    pub(crate) fn new(inner: Arc<SessionInner>, path: &str) -> Self {
        Self {
            inner,
            path: path.to_string(),
        }
    }

    fn proxy(&self) -> Proxy<'_, &SyncConnection> {
        Proxy::new(SERVICE_NAME, PATH, TIMEOUT, &*self.inner.connection)
    }

    dbus_interface!();
    dbus_default_interface!(INTERFACE);

    pub(crate) fn register_interface(cr: &mut Crossroads) -> IfaceToken<Arc<Self>> {
        cr.register(INTERFACE, |ib: &mut IfaceBuilder<Arc<Self>>| {
            ib.method_with_cr_async("Join", (), (), |ctx, cr, ()| {
                method_call(ctx, cr, move |_reg: Arc<Self>| async move {
                    println!("Join");
                    Ok(())
                })
            });
        })
    }

    pub(crate) async fn register(self, inner: Arc<SessionInner>) -> Result<()> {
        let mut cr = inner.crossroads.lock().await;
        let om = cr.object_manager();

        let path = self.path.clone();

        cr.insert(path, &[inner.application_token, om], Arc::new(self));

        Ok(())
    }

}