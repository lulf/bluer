//! Implement Application bluetooth mesh interface

use crate::{method_call, Result, SessionInner};
use std::sync::Arc;

use dbus::{
    nonblock::{Proxy, SyncConnection},
};
use dbus_crossroads::{Crossroads, IfaceBuilder, IfaceToken};

use crate::mesh::{SERVICE_NAME, PATH, TIMEOUT};
use futures::{channel::oneshot,};
use std::fmt;

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
            ib.method_with_cr_async("JoinComplete", ("token",), (), |ctx, cr, (_token,): (u64,)| {
                method_call(ctx, cr, move |_reg: Arc<Self>| async move {
                    println!("JoinComplete");
                    Ok(())
                })
            });
            ib.method_with_cr_async("JoinFailed", ("reason",), (), |ctx, cr, (_reason,): (String,)| {
                method_call(ctx, cr, move |_reg: Arc<Self>| async move {
                    println!("JoinFailed");
                    Ok(())
                })
            });
            cr_property!(ib, "CompanyID", _reg => {
                Some(0x05f1)
            });
            cr_property!(ib, "ProductID", _reg => {
                Some(0x0001)
            });
            cr_property!(ib, "VersionID", _reg => {
                Some(0x0001)
            });
        })
    }

    pub(crate) async fn register(self, inner: Arc<SessionInner>) -> Result<ApplicationHandle> {
        let app_path = dbus::Path::new(self.path.clone()).unwrap();

        {
            let mut cr = inner.crossroads.lock().await;
            let om = cr.object_manager();
            cr.insert(app_path.clone(), &[inner.application_token, om], Arc::new(self));
        }

        let (drop_tx, drop_rx) = oneshot::channel();
        let path_unreg = app_path.clone();
        tokio::spawn(async move {
            let _ = drop_rx.await;

            log::trace!("Unpublishing profile at {}", &path_unreg);
            let mut cr = inner.crossroads.lock().await;
            let _: Option<Self> = cr.remove(&path_unreg);
        });

        Ok(ApplicationHandle { name: app_path, _drop_tx: drop_tx })
    }

}

/// Handle to Application
///
/// Drop this handle to unpublish.
pub struct ApplicationHandle {
    name: dbus::Path<'static>,
    _drop_tx: oneshot::Sender<()>,
}

impl Drop for ApplicationHandle {
    fn drop(&mut self) {
        // required for drop order
    }
}

impl fmt::Debug for ApplicationHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ApplicationHandle {{ {} }}", &self.name)
    }
}