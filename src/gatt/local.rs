//! Local GATT services.

use dbus::{
    arg::{AppendAll, OwnedFd, PropMap, Variant},
    channel::Sender,
    message::SignalArgs,
    nonblock::{stdintf::org_freedesktop_dbus::PropertiesPropertiesChanged, Proxy, SyncConnection},
    MethodErr, Path,
};
use dbus_crossroads::{Context, Crossroads, IfaceBuilder, IfaceToken};
use futures::{channel::oneshot, lock::Mutex, Future, FutureExt};
use pin_project::pin_project;
use std::{
    collections::HashSet,
    fmt,
    marker::PhantomData,
    mem::take,
    num::NonZeroU16,
    pin::Pin,
    sync::{Arc, Weak},
    task::Poll,
};
use strum::IntoStaticStr;
use tokio::{
    io::{self, AsyncRead, AsyncWrite},
    net::UnixStream,
    sync::{mpsc, watch},
};
use uuid::Uuid;

use super::{
    CharacteristicDescriptorFlags, CharacteristicFlags, WriteValueType, CHARACTERISTIC_INTERFACE,
    DESCRIPTOR_INTERFACE, SERVICE_INTERFACE,
};
use crate::{
    make_socket_pair, parent_path, Adapter, Error, LinkType, Result, SessionInner, ERR_PREFIX, SERVICE_NAME,
    TIMEOUT,
};

pub(crate) const MANAGER_INTERFACE: &str = "org.bluez.GattManager1";

/// Call method on Arc D-Bus object we are serving.
fn method_call<T: Send + Sync + 'static, R: AppendAll, F: Future<Output = DbusResult<R>> + Send + 'static>(
    mut ctx: Context, cr: &mut Crossroads, f: impl FnOnce(Arc<T>) -> F,
) -> impl Future<Output = PhantomData<R>> {
    let data_ref: &mut Arc<T> = cr.data_mut(ctx.path()).unwrap();
    let data: Arc<T> = data_ref.clone();
    async move {
        let result = f(data).await;
        ctx.reply(result)
    }
}

// ===========================================================================================
// Request error
// ===========================================================================================

/// Error response from us to a Bluetooth request.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq, IntoStaticStr)]
pub enum Reject {
    #[error("Bluetooth request failed")]
    Failed,
    #[error("Bluetooth request already in progress")]
    InProgress,
    #[error("Invalid offset for Bluetooth GATT property")]
    InvalidOffset,
    #[error("Invalid value length for Bluetooth GATT property")]
    InvalidValueLength,
    #[error("Bluetooth request not permitted")]
    NotPermitted,
    #[error("Bluetooth request not authorized")]
    NotAuthorized,
    #[error("Bluetooth request not supported")]
    NotSupported,
}

impl Default for Reject {
    fn default() -> Self {
        Self::Failed
    }
}

impl From<Reject> for dbus::MethodErr {
    fn from(err: Reject) -> Self {
        let name: &'static str = err.clone().into();
        Self::from((ERR_PREFIX.to_string() + name, &err.to_string()))
    }
}

/// Result of a Bluetooth request to us.
pub type ReqResult<T> = std::result::Result<T, Reject>;

/// Result of calling one of our D-Bus methods.
type DbusResult<T> = std::result::Result<T, dbus::MethodErr>;

// ===========================================================================================
// Service
// ===========================================================================================

// ----------
// Definition
// ----------

/// Local GATT service exposed over Bluetooth.
#[derive(Debug, Default)]
pub struct Service {
    /// 128-bit service UUID.
    pub uuid: Uuid,
    /// Service handle.
    ///
    /// Set to `None` to auto allocate an available handle.
    pub handle: Option<NonZeroU16>,
    /// Indicates whether or not this GATT service is a
    /// primary service.
    ///
    /// If false, the service is secondary.
    pub primary: bool,
    /// List of GATT characteristics to expose.
    pub characteristics: Vec<Characteristic>,
    /// Control handle for service once it has been registered.
    pub control_handle: ServiceControlHandle,
}

// ----------
// Controller
// ----------

/// An object to control a service once it has been registered.
pub struct ServiceControl {
    handle_rx: watch::Receiver<Option<NonZeroU16>>,
}

impl fmt::Debug for ServiceControl {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ServiceControl {{ handle: {} }}", self.handle().map(|h| h.get()).unwrap_or_default())
    }
}

impl ServiceControl {
    /// Gets the assigned handle of the service.
    pub fn handle(&self) -> crate::Result<NonZeroU16> {
        match *self.handle_rx.borrow() {
            Some(handle) => Ok(handle),
            None => Err(Error::NotRegistered),
        }
    }
}

/// A handle to control a service once it has been registered.
pub struct ServiceControlHandle {
    handle_tx: watch::Sender<Option<NonZeroU16>>,
}

impl Default for ServiceControlHandle {
    fn default() -> Self {
        Self { handle_tx: watch::channel(None).0 }
    }
}

impl fmt::Debug for ServiceControlHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ServiceControlHandle")
    }
}

/// Creates a `ServiceControl` and its associated handle.
pub fn service_control() -> (ServiceControl, ServiceControlHandle) {
    let (handle_tx, handle_rx) = watch::channel(None);
    (ServiceControl { handle_rx }, ServiceControlHandle { handle_tx })
}

// ---------------
// D-Bus interface
// ---------------

/// A service exposed over D-Bus to bluez.
pub(crate) struct RegisteredService {
    s: Service,
}

impl RegisteredService {
    fn new(s: Service) -> Self {
        if let Some(handle) = s.handle {
            let _ = s.control_handle.handle_tx.send(Some(handle));
        }
        Self { s }
    }

    pub(crate) fn register_interface(cr: &mut Crossroads) -> IfaceToken<Arc<Self>> {
        cr.register(SERVICE_INTERFACE, |ib: &mut IfaceBuilder<Arc<Self>>| {
            cr_property!(ib, "UUID", reg => {
                Some(reg.s.uuid.to_string())
            });
            cr_property!(ib, "Primary", reg => {
                Some(reg.s.primary)
            });
            ib.property("Handle").get(|_ctx, reg| Ok(reg.s.handle.map(|h| h.get()).unwrap_or_default())).set(
                |_ctx, reg, v| {
                    let handle = NonZeroU16::new(v);
                    dbg!(&handle);
                    let _ = reg.s.control_handle.handle_tx.send(handle);
                    Ok(None)
                },
            );
        })
    }
}

// ===========================================================================================
// Characteristic
// ===========================================================================================

// ----------
// Definition
// ----------

/// Characteristic read value function.
pub type CharacteristicReadFun = Box<
    dyn (Fn(CharacteristicReadRequest) -> Pin<Box<dyn Future<Output = ReqResult<Vec<u8>>> + Send>>) + Send + Sync,
>;

/// Characteristic read.
#[derive(custom_debug::Debug)]
pub struct CharacteristicRead {
    /// If set allows clients to read this characteristic.
    pub read: bool,
    /// Require encryption.
    pub encrypt_read: bool,
    /// Require authentication.
    pub encrypt_authenticated_read: bool,
    /// Require security.
    pub secure_read: bool,
    /// Function called for each read request returning value.
    #[debug(skip)]
    pub fun: CharacteristicReadFun,
}

impl Default for CharacteristicRead {
    fn default() -> Self {
        Self {
            read: false,
            encrypt_read: false,
            encrypt_authenticated_read: false,
            secure_read: false,
            fun: Box::new(|_| async move { Err(Reject::NotSupported) }.boxed()),
        }
    }
}

impl CharacteristicRead {
    fn set_characteristic_flags(&self, f: &mut CharacteristicFlags) {
        f.read = self.read;
        f.encrypt_read = self.encrypt_read;
        f.encrypt_authenticated_read = self.encrypt_authenticated_read;
        f.secure_read = self.secure_read;
    }
}

/// Characteristic write value function.
pub type CharacteristicWriteFun = Box<
    dyn Fn(Vec<u8>, CharacteristicWriteRequest) -> Pin<Box<dyn Future<Output = ReqResult<()>> + Send>>
        + Send
        + Sync,
>;

/// Characteristic write value method.
pub enum CharacteristicWriteMethod {
    /// Call specified function for each write request.
    Fun(CharacteristicWriteFun),
    /// Provide written data over `AsyncRead` IO.
    ///
    /// Use `CharacteristicControlHandle` to obtain reader.
    Io,
}

impl fmt::Debug for CharacteristicWriteMethod {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Fun(_) => write!(f, "Fun"),
            Self::Io => write!(f, "Io"),
        }
    }
}

impl Default for CharacteristicWriteMethod {
    fn default() -> Self {
        Self::Fun(Box::new(|_, _| async move { Err(Reject::NotSupported) }.boxed()))
    }
}

/// Characteristic write.
#[derive(Debug, Default)]
pub struct CharacteristicWrite {
    /// If set allows clients to use the Write Command ATT operation.
    pub write: bool,
    /// If set allows clients to use the Write Request/Response operation.
    pub write_without_response: bool,
    /// If set allows clients to use the Reliable Writes procedure.
    pub reliable_write: bool,
    /// If set allows clients to use the Signed Write Without Response procedure.
    pub authenticated_signed_writes: bool,
    /// Require encryption.
    pub encrypt_write: bool,
    /// Require authentication.
    pub encrypt_authenticated_write: bool,
    /// Require security.
    pub secure_write: bool,
    /// Write value method.
    pub method: CharacteristicWriteMethod,
}

impl CharacteristicWrite {
    fn set_characteristic_flags(&self, f: &mut CharacteristicFlags) {
        f.write = self.write;
        f.write_without_response = self.write_without_response;
        f.reliable_write = self.reliable_write;
        f.authenticated_signed_writes = self.authenticated_signed_writes;
        f.encrypt_write = self.encrypt_write;
        f.encrypt_authenticated_write = self.encrypt_authenticated_write;
        f.secure_write = self.secure_write;
    }
}

/// Characteristic start notifications function.
///
/// This function cannot fail, since there is to way to provide an error response to the
/// requesting device.
pub type CharacteristicNotifyFun =
    Box<dyn Fn(CharacteristicNotifier) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Characteristic notify value method.
pub enum CharacteristicNotifyMethod {
    /// Call specified function when client starts a notification session.
    Fun(CharacteristicNotifyFun),
    /// Write notify data over `AsyncWrite` IO.
    ///
    /// Use `CharacteristicControlHandle` to obtain writer.
    Io,
}

impl Default for CharacteristicNotifyMethod {
    fn default() -> Self {
        Self::Fun(Box::new(|_| async move {}.boxed()))
    }
}

impl fmt::Debug for CharacteristicNotifyMethod {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Fun(_) => write!(f, "Fun"),
            Self::Io => write!(f, "Io"),
        }
    }
}

/// Characteristic notify.
#[derive(Debug, Default)]
pub struct CharacteristicNotify {
    /// If set allows the client to use the Handle Value Notification operation.
    pub notify: bool,
    /// If set allows the client to use the Handle Value Indication/Confirmation operation.
    ///
    /// Confirmations will only be provided when this is `true` and `notify` is `false`.
    pub indicate: bool,
    /// Notification and indication method.
    pub method: CharacteristicNotifyMethod,
}

impl CharacteristicNotify {
    fn set_characteristic_flags(&self, f: &mut CharacteristicFlags) {
        f.notify = self.notify;
        f.indicate = self.indicate;
    }
}

/// Local GATT characteristic exposed over Bluetooth.
#[derive(Default, Debug)]
pub struct Characteristic {
    /// 128-bit characteristic UUID.
    pub uuid: Uuid,
    /// Characteristic handle.
    ///
    /// Set to `None` to auto allocate an available handle.
    pub handle: Option<NonZeroU16>,
    /// If set, permits broadcasts of the Characteristic Value using
    /// Server Characteristic Configuration Descriptor.
    pub broadcast: bool,
    /// If set a client can write to the Characteristic User Description Descriptor.
    pub writable_auxiliaries: bool,
    /// Authorize flag.
    pub authorize: bool,
    /// Characteristic descriptors.
    pub descriptors: Vec<Descriptor>,
    /// Read value of characteristic.
    pub read: Option<CharacteristicRead>,
    /// Write value of characteristic.
    pub write: Option<CharacteristicWrite>,
    /// Notify client of characteristic value change.
    pub notify: Option<CharacteristicNotify>,
    /// Control handle for characteristic once it has been registered.
    pub control_handle: CharacteristicControlHandle,
}

impl Characteristic {
    fn set_characteristic_flags(&self, f: &mut CharacteristicFlags) {
        f.broadcast = self.broadcast;
        f.writable_auxiliaries = self.writable_auxiliaries;
        f.authorize = self.authorize;
    }
}

// ------------------
// Callback interface
// ------------------

/// Read value request.
#[derive(Debug, Clone)]
pub struct CharacteristicReadRequest {
    /// Offset.
    pub offset: u16,
    /// Exchanged MTU.
    pub mtu: u16,
    /// Link type.
    pub link: Option<LinkType>,
}

impl CharacteristicReadRequest {
    fn from_dict(dict: &PropMap) -> DbusResult<Self> {
        Ok(Self {
            offset: read_opt_prop!(dict, "offset", u16).unwrap_or_default(),
            mtu: read_prop!(dict, "mtu", u16),
            link: read_opt_prop!(dict, "link", String).and_then(|v| v.parse().ok()),
        })
    }
}

/// Write value request.
#[derive(Debug, Clone)]
pub struct CharacteristicWriteRequest {
    /// Start offset.
    pub offset: u16,
    /// Write operation type.
    pub op_type: WriteValueType,
    /// Exchanged MTU.
    pub mtu: u16,
    /// Link type.
    pub link: Option<LinkType>,
    /// True if prepare authorization request.
    pub prepare_authorize: bool,
}

impl CharacteristicWriteRequest {
    fn from_dict(dict: &PropMap) -> DbusResult<Self> {
        Ok(Self {
            offset: read_opt_prop!(dict, "offset", u16).unwrap_or_default(),
            op_type: read_opt_prop!(dict, "type", String)
                .map(|s| s.parse().map_err(|_| MethodErr::invalid_arg("type")))
                .transpose()?
                .unwrap_or_default(),
            mtu: read_prop!(dict, "mtu", u16),
            link: read_opt_prop!(dict, "link", String).and_then(|v| v.parse().ok()),
            prepare_authorize: read_opt_prop!(dict, "prepare-authorize", bool).unwrap_or_default(),
        })
    }
}

/// Notification session.
///
/// Use this to send notifications or indications.
pub struct CharacteristicNotifier {
    connection: Weak<SyncConnection>,
    path: Path<'static>,
    stop_notify_tx: mpsc::Sender<()>,
    confirm_rx: Option<mpsc::Receiver<()>>,
}

impl CharacteristicNotifier {
    /// True, if each notification is confirmed by the receiving device.
    ///
    /// This is the case when the Indication mechanism is used.
    pub fn confirming(&self) -> bool {
        self.confirm_rx.is_some()
    }

    /// True, if the notification session has been stopped by the receiving device.
    pub fn is_stopped(&self) -> bool {
        self.stop_notify_tx.is_closed()
    }

    /// Resolves once the notification sesstion has been stopped by the receiving device.
    pub fn stopped(&self) -> impl Future<Output = ()> {
        let stop_notify_tx = self.stop_notify_tx.clone();
        async move { stop_notify_tx.closed().await }
    }

    /// Sends a notification or indication with the specified data to the receiving device.
    ///
    /// If `confirming` is true, the function waits until a confirmation is received from
    /// the device before it returns.
    ///
    /// This fails when the notification session has been stopped by the receiving device.
    pub async fn notify(&mut self, value: Vec<u8>) -> Result<()> {
        let connection = self.connection.upgrade().ok_or(Error::DBusConnectionLost)?;
        if self.is_stopped() {
            return Err(Error::NotificationSessionStopped);
        }

        // Flush confirmation queue.
        // This is necessary because previous notify call could have been aborted
        // before receiving the confirmation.
        if let Some(confirm_rx) = &mut self.confirm_rx {
            while let Some(Some(())) = confirm_rx.recv().now_or_never() {}
        }

        // Send notification.
        let mut changed_properties = PropMap::new();
        changed_properties.insert("Value".to_string(), Variant(Box::new(value)));
        let ppc = PropertiesPropertiesChanged {
            interface_name: CHARACTERISTIC_INTERFACE.to_string(),
            changed_properties,
            invalidated_properties: Vec::new(),
        };
        let msg = ppc.to_emit_message(&self.path);
        connection.send(msg).map_err(|_| Error::DBusConnectionLost)?;
        drop(connection);

        // Wait for confirmation if this is an indication session.
        // Note that we can be aborted before we receive the confirmation.
        if let Some(confirm_rx) = &mut self.confirm_rx {
            match confirm_rx.recv().await {
                Some(()) => Ok(()),
                None => Err(Error::IndicationUnconfirmed),
            }
        } else {
            Ok(())
        }
    }
}

// ------------
// IO interface
// ------------

/// A remote request to start writing to a characteristic via IO.
pub struct CharacteristicWriteIoRequest {
    mtu: u16,
    link: Option<LinkType>,
    tx: oneshot::Sender<ReqResult<OwnedFd>>,
}

impl CharacteristicWriteIoRequest {
    /// Maximum transmission unit.
    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    /// Link type.
    pub fn link(&self) -> Option<LinkType> {
        self.link
    }

    /// Accept the write request.
    pub fn accept(self) -> Result<CharacteristicReader> {
        let CharacteristicWriteIoRequest { mtu, link, tx } = self;
        let (fd, stream) = make_socket_pair()?;
        let _ = tx.send(Ok(fd));
        Ok(CharacteristicReader { mtu: mtu.into(), link, stream })
    }

    /// Reject the write request.
    pub fn reject(self, reason: Reject) {
        let _ = self.tx.send(Err(reason));
    }
}

/// Provides write requests to a characteristic as an IO stream.
#[pin_project]
pub struct CharacteristicReader {
    mtu: usize,
    link: Option<LinkType>,
    #[pin]
    stream: UnixStream,
}

impl CharacteristicReader {
    /// Maximum transmission unit.
    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// Link type.
    pub fn link(&self) -> Option<LinkType> {
        self.link
    }

    /// Gets the underlying UNIX socket.
    pub fn get(&self) -> &UnixStream {
        &self.stream
    }

    /// Gets the underlying UNIX socket mutably.
    pub fn get_mut(&mut self) -> &mut UnixStream {
        &mut self.stream
    }

    /// Transforms the reader into the underlying UNIX socket.
    pub fn into_inner(self) -> UnixStream {
        self.stream
    }
}

impl fmt::Debug for CharacteristicReader {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "CharacteristicReader {{ {:?} }}", &self.stream)
    }
}

impl AsyncRead for CharacteristicReader {
    fn poll_read(
        self: Pin<&mut Self>, cx: &mut std::task::Context, buf: &mut io::ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        self.project().stream.poll_read(cx, buf)
    }
}

/// Allows sending of notifications of a characteristic via an IO stream.
#[pin_project]
pub struct CharacteristicWriter {
    mtu: usize,
    link: Option<LinkType>,
    #[pin]
    stream: UnixStream,
}

impl CharacteristicWriter {
    /// Maximum transmission unit.
    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// Link type.
    pub fn link(&self) -> Option<LinkType> {
        self.link
    }

    /// Gets the underlying UNIX socket.
    pub fn get(&self) -> &UnixStream {
        &self.stream
    }

    /// Gets the underlying UNIX socket mutably.
    pub fn get_mut(&mut self) -> &mut UnixStream {
        &mut self.stream
    }

    /// Transforms the reader into the underlying UNIX socket.
    pub fn into_inner(self) -> UnixStream {
        self.stream
    }
}

impl fmt::Debug for CharacteristicWriter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "CharacteristicWriter {{ {:?} }}", &self.stream)
    }
}

impl AsyncWrite for CharacteristicWriter {
    fn poll_write(self: Pin<&mut Self>, cx: &mut std::task::Context, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        self.project().stream.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut std::task::Context) -> Poll<std::io::Result<()>> {
        self.project().stream.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut std::task::Context) -> Poll<std::io::Result<()>> {
        self.project().stream.poll_shutdown(cx)
    }
}

// ----------
// Controller
// ----------

/// An object to control a characteristic once it has been registered.
pub struct CharacteristicControl {
    handle_rx: watch::Receiver<Option<NonZeroU16>>,
    write_request_rx: mpsc::Receiver<CharacteristicWriteIoRequest>,
    notify_writer_rx: mpsc::Receiver<CharacteristicWriter>,
}

impl fmt::Debug for CharacteristicControl {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "CharacteristicControl {{ handle: {} }}", self.handle().map(|h| h.get()).unwrap_or_default())
    }
}

impl CharacteristicControl {
    /// Gets the assigned handle of the characteristic.
    pub fn handle(&self) -> crate::Result<NonZeroU16> {
        match *self.handle_rx.borrow() {
            Some(handle) => Ok(handle),
            None => Err(Error::NotRegistered),
        }
    }

    /// When using the write IO method, gets the next request to start writing to the characteristic.
    pub async fn write_request(&mut self) -> Result<CharacteristicWriteIoRequest> {
        match self.write_request_rx.recv().await {
            Some(req) => Ok(req),
            None => Err(Error::NotRegistered),
        }
    }

    /// When using the notify IO method, gets the next notification session.
    ///
    /// Note that bluez acknowledges the client's request before notifying us
    /// of the start of the notification session.
    pub async fn notifier(&mut self) -> Result<CharacteristicWriter> {
        match self.notify_writer_rx.recv().await {
            Some(writer) => Ok(writer),
            None => Err(Error::NotRegistered),
        }
    }
}

/// A handle to control a characteristic once it has been registered.
pub struct CharacteristicControlHandle {
    handle_tx: watch::Sender<Option<NonZeroU16>>,
    write_request_tx: Option<mpsc::Sender<CharacteristicWriteIoRequest>>,
    notify_writer_tx: Option<mpsc::Sender<CharacteristicWriter>>,
}

impl Default for CharacteristicControlHandle {
    fn default() -> Self {
        Self { handle_tx: watch::channel(None).0, write_request_tx: None, notify_writer_tx: None }
    }
}

impl fmt::Debug for CharacteristicControlHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "CharacteristicControlHandle")
    }
}

/// Creates a `CharacteristicControl` and its associated handle.
pub fn characteristic_control() -> (CharacteristicControl, CharacteristicControlHandle) {
    let (handle_tx, handle_rx) = watch::channel(None);
    let (write_request_tx, write_request_rx) = mpsc::channel(0);
    let (notify_writer_tx, notify_writer_rx) = mpsc::channel(0);
    (
        CharacteristicControl { handle_rx, write_request_rx, notify_writer_rx },
        CharacteristicControlHandle {
            handle_tx,
            write_request_tx: Some(write_request_tx),
            notify_writer_tx: Some(notify_writer_tx),
        },
    )
}

// ---------------
// D-Bus interface
// ---------------

/// Characteristic acquire write or notify request.
#[derive(Debug, Clone)]
struct CharacteristicAcquireRequest {
    /// Exchanged MTU.
    pub mtu: u16,
    /// Link type.
    pub link: Option<LinkType>,
}

impl CharacteristicAcquireRequest {
    fn from_dict(dict: &PropMap) -> DbusResult<Self> {
        Ok(Self {
            mtu: read_prop!(dict, "mtu", u16),
            link: read_opt_prop!(dict, "link", String).and_then(|v| v.parse().ok()),
        })
    }
}

/// Notification state of a registered characteristic.
struct CharacteristicNotifyState {
    confirm_tx: Option<mpsc::Sender<()>>,
    _stop_notify_rx: mpsc::Receiver<()>,
}

/// A characteristic exposed over D-Bus to bluez.
pub(crate) struct RegisteredCharacteristic {
    c: Characteristic,
    notify: Mutex<Option<CharacteristicNotifyState>>,
    connection: Weak<SyncConnection>,
}

impl RegisteredCharacteristic {
    fn new(mut c: Characteristic, connection: &Arc<SyncConnection>) -> Self {
        if let Some(handle) = c.handle {
            let _ = c.control_handle.handle_tx.send(Some(handle));
        }
        match &c.notify {
            Some(CharacteristicNotify { method: CharacteristicNotifyMethod::Io, .. }) => (),
            _ => c.control_handle.notify_writer_tx = None,
        }
        match &c.write {
            Some(CharacteristicWrite { method: CharacteristicWriteMethod::Io, .. }) => (),
            _ => c.control_handle.write_request_tx = None,
        }
        Self { c, notify: Mutex::new(None), connection: Arc::downgrade(&connection) }
    }

    pub(crate) fn register_interface(cr: &mut Crossroads) -> IfaceToken<Arc<Self>> {
        cr.register(CHARACTERISTIC_INTERFACE, |ib: &mut IfaceBuilder<Arc<Self>>| {
            cr_property!(ib, "UUID", reg => {
                Some(reg.c.uuid.to_string())
            });
            cr_property!(ib, "Flags", reg => {
                let mut flags = CharacteristicFlags::default();
                reg.c.set_characteristic_flags(&mut flags);
                if let Some(read) = &reg.c.read {
                    read.set_characteristic_flags(&mut flags);
                }
                if let Some(write) = &reg.c.write {
                    write.set_characteristic_flags(&mut flags);
                }
                if let Some(notify) = &reg.c.notify {
                    notify.set_characteristic_flags(&mut flags);
                }
                Some(flags.to_vec())
            });
            ib.property("Service").get(|ctx, _| Ok(parent_path(ctx.path())));
            ib.property("Handle").get(|_ctx, reg| Ok(reg.c.handle.map(|h| h.get()).unwrap_or_default())).set(
                |_ctx, reg, v| {
                    let handle = NonZeroU16::new(v);
                    dbg!(&handle);
                    let _ = reg.c.control_handle.handle_tx.send(handle);
                    Ok(None)
                },
            );
            cr_property!(ib, "WriteAcquired", reg => {
                if reg.c.control_handle.write_request_tx.is_some() {
                    Some(false)
                } else {
                    None
                }
            });
            cr_property!(ib, "NotifyAcquired", reg => {
                if reg.c.control_handle.notify_writer_tx.is_some() {
                    Some(false)
                } else {
                    None
                }
            });
            ib.method_with_cr_async("ReadValue", ("options",), ("value",), |ctx, cr, (options,): (PropMap,)| {
                method_call(ctx, cr, |reg: Arc<Self>| async move {
                    let options = CharacteristicReadRequest::from_dict(&options)?;
                    match &reg.c.read {
                        Some(read) => {
                            let value = (read.fun)(options).await?;
                            Ok((value,))
                        }
                        None => Err(Reject::NotSupported.into()),
                    }
                })
            });
            ib.method_with_cr_async(
                "WriteValue",
                ("value", "options"),
                (),
                |ctx, cr, (value, options): (Vec<u8>, PropMap)| {
                    method_call(ctx, cr, |reg: Arc<Self>| async move {
                        let options = CharacteristicWriteRequest::from_dict(&options)?;
                        match &reg.c.write {
                            Some(CharacteristicWrite { method: CharacteristicWriteMethod::Fun(fun), .. }) => {
                                fun(value, options).await?;
                                Ok(())
                            }
                            _ => Err(Reject::NotSupported.into()),
                        }
                    })
                },
            );
            ib.method_with_cr_async("StartNotify", (), (), |ctx, cr, ()| {
                let path = ctx.path().clone();
                method_call(ctx, cr, |reg: Arc<Self>| async move {
                    match &reg.c.notify {
                        Some(CharacteristicNotify {
                            method: CharacteristicNotifyMethod::Fun(notify_fn),
                            indicate,
                            notify,
                        }) => {
                            let (stop_notify_tx, stop_notify_rx) = mpsc::channel(0);
                            let (confirm_tx, confirm_rx) = if *indicate && !*notify {
                                let (tx, rx) = mpsc::channel(0);
                                (Some(tx), Some(rx))
                            } else {
                                (None, None)
                            };
                            {
                                let mut notify = reg.notify.lock().await;
                                *notify = Some(CharacteristicNotifyState {
                                    _stop_notify_rx: stop_notify_rx,
                                    confirm_tx,
                                });
                            }
                            let notifier = CharacteristicNotifier {
                                connection: reg.connection.clone(),
                                path,
                                stop_notify_tx,
                                confirm_rx,
                            };
                            notify_fn(notifier).await;
                            Ok(())
                        }
                        _ => Err(Reject::NotSupported.into()),
                    }
                })
            });
            ib.method_with_cr_async("StopNotify", (), (), |ctx, cr, ()| {
                method_call(ctx, cr, |reg: Arc<Self>| async move {
                    let mut notify = reg.notify.lock().await;
                    *notify = None;
                    Ok(())
                })
            });
            ib.method_with_cr_async("Confirm", (), (), |ctx, cr, ()| {
                method_call(ctx, cr, |reg: Arc<Self>| async move {
                    let mut notify = reg.notify.lock().await;
                    if let Some(CharacteristicNotifyState { confirm_tx: Some(confirm_tx), .. }) = &mut *notify {
                        let _ = confirm_tx.send(()).await;
                    }
                    Ok(())
                })
            });
            ib.method_with_cr_async(
                "AcquireWrite",
                ("options",),
                ("fd", "mtu"),
                |ctx, cr, (options,): (PropMap,)| {
                    method_call(ctx, cr, |reg: Arc<Self>| async move {
                        let options = CharacteristicAcquireRequest::from_dict(&options)?;
                        match &reg.c.control_handle.write_request_tx {
                            Some(write_request_tx) => {
                                let (tx, rx) = oneshot::channel();
                                let req =
                                    CharacteristicWriteIoRequest { mtu: options.mtu, link: options.link, tx };
                                write_request_tx.send(req).await.map_err(|_| Reject::Failed)?;
                                let fd = rx.await.map_err(|_| Reject::Failed)??;
                                Ok((fd, options.mtu))
                            }
                            None => Err(Reject::NotSupported.into()),
                        }
                    })
                },
            );
            ib.method_with_cr_async(
                "AcquireNotify",
                ("options",),
                ("fd", "mtu"),
                |ctx, cr, (options,): (PropMap,)| {
                    method_call(ctx, cr, |reg: Arc<Self>| async move {
                        let options = CharacteristicAcquireRequest::from_dict(&options)?;
                        match &reg.c.control_handle.notify_writer_tx {
                            Some(notify_writer_tx) => {
                                // bluez has already confirmed the start of the notification session.
                                // So there is no point in making this failable by our users.
                                let (fd, stream) = make_socket_pair().map_err(|_| Reject::Failed)?;
                                let writer =
                                    CharacteristicWriter { mtu: options.mtu.into(), link: options.link, stream };
                                let _ = notify_writer_tx.send(writer).await;
                                Ok((fd, options.mtu))
                            }
                            None => Err(Reject::NotSupported.into()),
                        }
                    })
                },
            );
        })
    }
}

// ===========================================================================================
// Characteristic descriptor
// ===========================================================================================

// ----------
// Definition
// ----------

/// Characteristic descriptor read value function.
pub type DescriptorReadFun =
    Box<dyn Fn(ReadDescriptorRequest) -> Pin<Box<dyn Future<Output = ReqResult<Vec<u8>>> + Send>> + Send + Sync>;

/// Characteristic descriptor read.
#[derive(custom_debug::Debug)]
pub struct DescriptorRead {
    /// If set allows clients to read this characteristic.
    pub read: bool,
    /// Require encryption.
    pub encrypt_read: bool,
    /// Require authentication.
    pub encrypt_authenticated_read: bool,
    /// Require security.
    pub secure_read: bool,
    /// Function called for each read request returning value.
    #[debug(skip)]
    pub fun: DescriptorReadFun,
}

impl Default for DescriptorRead {
    fn default() -> Self {
        Self {
            read: false,
            encrypt_read: false,
            encrypt_authenticated_read: false,
            secure_read: false,
            fun: Box::new(|_| async move { Err(Reject::NotSupported) }.boxed()),
        }
    }
}

impl DescriptorRead {
    fn set_descriptor_flags(&self, f: &mut CharacteristicDescriptorFlags) {
        f.read = self.read;
        f.encrypt_read = self.encrypt_read;
        f.encrypt_authenticated_read = self.encrypt_authenticated_read;
        f.secure_read = self.secure_read;
    }
}

/// Characteristic descriptor write value function.
pub type DescriptorWriteFun = Box<
    dyn Fn(Vec<u8>, WriteDescriptorRequest) -> Pin<Box<dyn Future<Output = ReqResult<()>> + Send>> + Send + Sync,
>;

/// Characteristic write.
#[derive(custom_debug::Debug)]
pub struct DescriptorWrite {
    /// If set allows clients to use the Write Command ATT operation.
    pub write: bool,
    /// Require encryption.
    pub encrypt_write: bool,
    /// Require authentication.
    pub encrypt_authenticated_write: bool,
    /// Require security.
    pub secure_write: bool,
    /// Function called for each write request.
    #[debug(skip)]
    pub fun: DescriptorWriteFun,
}

impl Default for DescriptorWrite {
    fn default() -> Self {
        Self {
            write: false,
            encrypt_write: false,
            encrypt_authenticated_write: false,
            secure_write: false,
            fun: Box::new(|_, _| async move { Err(Reject::NotSupported) }.boxed()),
        }
    }
}

impl DescriptorWrite {
    fn set_descriptor_flags(&self, f: &mut CharacteristicDescriptorFlags) {
        f.write = self.write;
        f.encrypt_write = self.encrypt_write;
        f.encrypt_authenticated_write = self.encrypt_authenticated_write;
        f.secure_write = self.secure_write;
    }
}

/// Local GATT characteristic descriptor exposed over Bluetooth.
#[derive(Default, Debug)]
pub struct Descriptor {
    /// 128-bit descriptor UUID.
    pub uuid: Uuid,
    /// Characteristic descriptor handle.
    ///
    /// Set to `None` to auto allocate an available handle.
    pub handle: Option<NonZeroU16>,
    /// Authorize flag.
    pub authorize: bool,
    /// Read value of characteristic descriptor.
    pub read: Option<DescriptorRead>,
    /// Write value of characteristic descriptor.
    pub write: Option<DescriptorWrite>,
    /// Control handle for characteristic descriptor once it has been registered.
    pub control_handle: DescriptorControlHandle,
}

impl Descriptor {
    fn set_descriptor_flags(&self, f: &mut CharacteristicDescriptorFlags) {
        f.authorize = self.authorize;
    }
}

// ------------------
// Callback interface
// ------------------

/// Read characteristic descriptor value request.
#[derive(Debug, Clone)]
pub struct ReadDescriptorRequest {
    /// Offset.
    pub offset: u16,
    /// Link type.
    pub link: Option<LinkType>,
}

impl ReadDescriptorRequest {
    fn from_dict(dict: &PropMap) -> DbusResult<Self> {
        Ok(Self {
            offset: read_prop!(dict, "offset", u16),
            link: read_opt_prop!(dict, "link", String).and_then(|v| v.parse().ok()),
        })
    }
}

/// Write characteristic descriptor value request.
#[derive(Debug, Clone)]
pub struct WriteDescriptorRequest {
    /// Offset.
    pub offset: u16,
    /// Link type.
    pub link: Option<LinkType>,
    /// Is prepare authorization request?
    pub prepare_authorize: bool,
}

impl WriteDescriptorRequest {
    fn from_dict(dict: &PropMap) -> DbusResult<Self> {
        Ok(Self {
            offset: read_prop!(dict, "offset", u16),
            link: read_opt_prop!(dict, "link", String).and_then(|v| v.parse().ok()),
            prepare_authorize: read_prop!(dict, "prepare_authorize", bool),
        })
    }
}

// ----------
// Controller
// ----------

/// An object to control a characteristic descriptor once it has been registered.
pub struct DescriptorControl {
    handle_rx: watch::Receiver<Option<NonZeroU16>>,
}

impl fmt::Debug for DescriptorControl {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "DescriptorControl {{ handle: {} }}", self.handle().map(|h| h.get()).unwrap_or_default())
    }
}

impl DescriptorControl {
    /// Gets the assigned handle of the characteristic descriptor.
    pub fn handle(&self) -> crate::Result<NonZeroU16> {
        match *self.handle_rx.borrow() {
            Some(handle) => Ok(handle),
            None => Err(Error::NotRegistered),
        }
    }
}

/// A handle to control a characteristic descriptor once it has been registered.
pub struct DescriptorControlHandle {
    handle_tx: watch::Sender<Option<NonZeroU16>>,
}

impl Default for DescriptorControlHandle {
    fn default() -> Self {
        Self { handle_tx: watch::channel(None).0 }
    }
}

impl fmt::Debug for DescriptorControlHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "DescriptorControlHandle")
    }
}

/// Creates a `DescriptorControl` and its associated handle.
pub fn descriptor_control() -> (DescriptorControl, DescriptorControlHandle) {
    let (handle_tx, handle_rx) = watch::channel(None);
    (DescriptorControl { handle_rx }, DescriptorControlHandle { handle_tx })
}

// ---------------
// D-Bus interface
// ---------------

/// A characteristic descriptor exposed over D-Bus to bluez.
pub(crate) struct RegisteredDescriptor {
    d: Descriptor,
}

impl RegisteredDescriptor {
    fn new(d: Descriptor) -> Self {
        if let Some(handle) = d.handle {
            let _ = d.control_handle.handle_tx.send(Some(handle));
        }
        Self { d }
    }

    pub(crate) fn register_interface(cr: &mut Crossroads) -> IfaceToken<Arc<Self>> {
        cr.register(DESCRIPTOR_INTERFACE, |ib: &mut IfaceBuilder<Arc<Self>>| {
            cr_property!(ib, "UUID", reg => {
                Some(reg.d.uuid.to_string())
            });
            cr_property!(ib, "Flags", reg => {
                let mut flags = CharacteristicDescriptorFlags::default();
                reg.d.set_descriptor_flags(&mut flags);
                if let Some(read) = &reg.d.read {
                    read.set_descriptor_flags(&mut flags);
                }
                if let Some(write) = &reg.d.write {
                    write.set_descriptor_flags(&mut flags);
                }
                Some(flags.to_vec())
            });
            ib.property("Characteristic").get(|ctx, _| Ok(parent_path(ctx.path())));
            ib.property("Handle").get(|_ctx, reg| Ok(reg.d.handle.map(|h| h.get()).unwrap_or_default())).set(
                |_ctx, reg, v| {
                    let handle = NonZeroU16::new(v);
                    dbg!(&handle);
                    let _ = reg.d.control_handle.handle_tx.send(handle);
                    Ok(None)
                },
            );
            ib.method_with_cr_async("ReadValue", ("flags",), ("value",), |ctx, cr, (flags,): (PropMap,)| {
                method_call(ctx, cr, |reg: Arc<Self>| async move {
                    let options = ReadDescriptorRequest::from_dict(&flags)?;
                    match &reg.d.read {
                        Some(read) => {
                            let value = (read.fun)(options).await?;
                            Ok((value,))
                        }
                        None => Err(Reject::NotSupported.into()),
                    }
                })
            });
            ib.method_with_cr_async(
                "WriteValue",
                ("value", "flags"),
                (),
                |ctx, cr, (value, flags): (Vec<u8>, PropMap)| {
                    method_call(ctx, cr, |reg: Arc<Self>| async move {
                        let options = WriteDescriptorRequest::from_dict(&flags)?;
                        match &reg.d.write {
                            Some(write) => {
                                (write.fun)(value, options).await?;
                                Ok(())
                            }
                            None => Err(Reject::NotSupported.into()),
                        }
                    })
                },
            );
        })
    }
}

// ===========================================================================================
// Application
// ===========================================================================================

pub(crate) const GATT_APP_PREFIX: &str = "/io/crates/tokio_bluez/gatt/app/";

/// Local GATT application to publish over Bluetooth.
#[derive(Debug)]
pub struct Application {
    /// Services to publish.
    pub services: Vec<Service>,
}

impl Application {
    pub(crate) async fn register(
        mut self, inner: Arc<SessionInner>, adapter_name: Arc<String>,
    ) -> crate::Result<ApplicationHandle> {
        let mut reg_paths = Vec::new();
        let app_path = format!("{}{}", GATT_APP_PREFIX, Uuid::new_v4().to_simple());
        let app_path = dbus::Path::new(app_path).unwrap();

        {
            let mut cr = inner.crossroads.lock().await;

            let services = take(&mut self.services);
            reg_paths.push(app_path.clone());
            let om = cr.object_manager::<Self>();
            cr.insert(app_path.clone(), &[om], self);

            for (service_idx, mut service) in services.into_iter().enumerate() {
                let chars = take(&mut service.characteristics);

                let reg_service = RegisteredService::new(service);
                let service_path = format!("{}/service{}", &app_path, service_idx);
                let service_path = dbus::Path::new(service_path).unwrap();
                reg_paths.push(service_path.clone());
                cr.insert(service_path.clone(), &[inner.gatt_reg_service_token], Arc::new(reg_service));

                for (char_idx, mut char) in chars.into_iter().enumerate() {
                    let descs = take(&mut char.descriptors);

                    let reg_char = RegisteredCharacteristic::new(char, &inner.connection);
                    let char_path = format!("{}/char{}", &service_path, char_idx);
                    let char_path = dbus::Path::new(char_path).unwrap();
                    reg_paths.push(char_path.clone());
                    cr.insert(char_path.clone(), &[inner.gatt_reg_characteristic_token], Arc::new(reg_char));

                    for (desc_idx, desc) in descs.into_iter().enumerate() {
                        let reg_desc = RegisteredDescriptor::new(desc);
                        let desc_path = format!("{}/desc{}", &char_path, desc_idx);
                        let desc_path = dbus::Path::new(desc_path).unwrap();
                        reg_paths.push(desc_path.clone());
                        cr.insert(
                            desc_path,
                            &[inner.gatt_reg_characteristic_descriptor_token],
                            Arc::new(reg_desc),
                        );
                    }
                }
            }
        }

        let proxy =
            Proxy::new(SERVICE_NAME, Adapter::dbus_path(&*adapter_name)?, TIMEOUT, inner.connection.clone());
        dbg!(&app_path);
        //future::pending::<()>().await;
        proxy.method_call(MANAGER_INTERFACE, "RegisterApplication", (app_path.clone(), PropMap::new())).await?;

        let (drop_tx, drop_rx) = oneshot::channel();
        let app_path_unreg = app_path.clone();
        tokio::spawn(async move {
            let _ = drop_rx.await;
            let _: std::result::Result<(), dbus::Error> =
                proxy.method_call(MANAGER_INTERFACE, "UnregisterApplication", (app_path_unreg,)).await;

            let mut cr = inner.crossroads.lock().await;
            for reg_path in reg_paths {
                let _: Option<Self> = cr.remove(&reg_path);
            }
        });

        Ok(ApplicationHandle { name: app_path, _drop_tx: drop_tx })
    }
}

/// Local GATT application published over Bluetooth.
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

// ===========================================================================================
// GATT profile
// ===========================================================================================

pub(crate) const GATT_PROFILE_PREFIX: &str = "/io/crates/tokio_bluez/gatt/profile/";

/// Local profile (GATT client) instance.
///
/// By registering this type of object
/// an application effectively indicates support for a specific GATT profile
/// and requests automatic connections to be established to devices
/// supporting it.
#[derive(Debug, Clone)]
pub struct Profile {
    /// 128-bit GATT service UUIDs to auto connect.
    pub uuids: HashSet<Uuid>,
}

impl Profile {
    pub(crate) fn register_interface(cr: &mut Crossroads) -> IfaceToken<Self> {
        cr.register("org.bluez.GattProfile1", |ib: &mut IfaceBuilder<Self>| {
            cr_property!(ib, "UUIDs", p => {
                Some(p.uuids.iter().map(|uuid| uuid.to_string()).collect::<Vec<_>>())
            });
        })
    }

    pub(crate) async fn register(
        self, inner: Arc<SessionInner>, adapter_name: Arc<String>,
    ) -> crate::Result<ProfileHandle> {
        let profile_path = format!("{}{}", GATT_PROFILE_PREFIX, Uuid::new_v4().to_simple());
        let profile_path = dbus::Path::new(profile_path).unwrap();

        {
            let mut cr = inner.crossroads.lock().await;
            let om = cr.object_manager::<Self>();
            cr.insert(profile_path.clone(), &[inner.gatt_profile_token, om], self);
        }

        let proxy =
            Proxy::new(SERVICE_NAME, Adapter::dbus_path(&*adapter_name)?, TIMEOUT, inner.connection.clone());
        dbg!(&profile_path);
        //future::pending::<()>().await;
        proxy
            .method_call(MANAGER_INTERFACE, "RegisterApplication", (profile_path.clone(), PropMap::new()))
            .await?;

        let (drop_tx, drop_rx) = oneshot::channel();
        let profile_path_unreg = profile_path.clone();
        tokio::spawn(async move {
            let _ = drop_rx.await;

            let _: std::result::Result<(), dbus::Error> = proxy
                .method_call(MANAGER_INTERFACE, "UnregisterApplication", (profile_path_unreg.clone(),))
                .await;

            let mut cr = inner.crossroads.lock().await;
            let _: Option<Self> = cr.remove(&profile_path_unreg);
        });

        Ok(ProfileHandle { name: profile_path, _drop_tx: drop_tx })
    }
}

/// Published local profile (GATT client) instance.
///
/// Drop this handle to unpublish.
pub struct ProfileHandle {
    name: dbus::Path<'static>,
    _drop_tx: oneshot::Sender<()>,
}

impl Drop for ProfileHandle {
    fn drop(&mut self) {
        // required for drop order
    }
}

impl fmt::Debug for ProfileHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ProfileHandle {{ {} }}", &self.name)
    }
}