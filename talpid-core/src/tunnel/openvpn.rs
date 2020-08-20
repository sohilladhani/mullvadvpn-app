use super::TunnelEvent;
use crate::{
    mktemp,
    process::{
        openvpn::{OpenVpnCommand, OpenVpnProcHandle},
        stoppable_process::StoppableProcess,
    },
    proxy::{self, ProxyMonitor, ProxyResourceData},
};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use talpid_types::net::openvpn;
use tokio::task;
#[cfg(target_os = "linux")]
use which;


/// Results from fallible operations on the OpenVPN tunnel.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can happen when using the OpenVPN tunnel.
#[derive(err_derive::Error, Debug)]
#[error(no_from)]
pub enum Error {
    /// Failed to initialize the tokio runtime.
    #[error(display = "Failed to initialize the tokio runtime")]
    RuntimeError(#[error(source)] io::Error),

    /// Unable to start, wait for or kill the OpenVPN process.
    #[error(display = "Error in OpenVPN process management: {}", _0)]
    ChildProcessError(&'static str, #[error(source)] io::Error),

    /// Unable to start the IPC server.
    #[error(display = "Unable to start the event dispatcher IPC server")]
    EventDispatcherError(#[error(source)] event_server::Error),

    /// The OpenVPN event dispatcher exited unexpectedly
    #[error(display = "The OpenVPN event dispatcher exited unexpectedly")]
    EventDispatcherExited,

    /// No TAP adapter was detected
    #[cfg(windows)]
    #[error(display = "No TAP adapter was detected")]
    MissingTapAdapter,

    /// TAP adapter seems to be disabled
    #[cfg(windows)]
    #[error(display = "The TAP adapter appears to be disabled")]
    DisabledTapAdapter,

    /// OpenVPN process died unexpectedly
    #[error(display = "OpenVPN process died unexpectedly")]
    ChildProcessDied,

    /// The IP routing program was not found.
    #[cfg(target_os = "linux")]
    #[error(display = "The IP routing program `ip` was not found")]
    IpRouteNotFound(#[error(source)] which::Error),

    /// The OpenVPN binary was not found.
    #[error(display = "No OpenVPN binary found at {}", _0)]
    OpenVpnNotFound(String),

    /// The OpenVPN plugin was not found.
    #[error(display = "No OpenVPN plugin found at {}", _0)]
    PluginNotFound(String),

    /// Error while writing credentials to temporary file.
    #[error(display = "Error while writing credentials to temporary file")]
    CredentialsWriteError(#[error(source)] io::Error),

    /// Failures related to the proxy service.
    #[error(display = "Unable to start the proxy service")]
    StartProxyError(#[error(source)] io::Error),

    /// Error while monitoring proxy service
    #[error(display = "Error while monitoring proxy service")]
    MonitorProxyError(#[error(source)] io::Error),

    /// The proxy exited unexpectedly
    #[error(
        display = "The proxy exited unexpectedly providing these details: {}",
        _0
    )]
    ProxyExited(String),

    /// Failure in Windows syscall.
    #[cfg(windows)]
    #[error(display = "Failure in Windows syscall")]
    WinnetError(#[error(source)] crate::winnet::Error),
}


#[cfg(unix)]
static OPENVPN_DIE_TIMEOUT: Duration = Duration::from_secs(4);
#[cfg(windows)]
static OPENVPN_DIE_TIMEOUT: Duration = Duration::from_secs(30);


#[cfg(target_os = "macos")]
const OPENVPN_PLUGIN_FILENAME: &str = "libtalpid_openvpn_plugin.dylib";
#[cfg(any(target_os = "linux", target_os = "android"))]
const OPENVPN_PLUGIN_FILENAME: &str = "libtalpid_openvpn_plugin.so";
#[cfg(windows)]
const OPENVPN_PLUGIN_FILENAME: &str = "talpid_openvpn_plugin.dll";

#[cfg(unix)]
const OPENVPN_BIN_FILENAME: &str = "openvpn";
#[cfg(windows)]
const OPENVPN_BIN_FILENAME: &str = "openvpn.exe";

/// Struct for monitoring an OpenVPN process.
#[derive(Debug)]
pub struct OpenVpnMonitor<C: OpenVpnBuilder = OpenVpnCommand> {
    child: Arc<C::ProcessHandle>,
    proxy_monitor: Option<Box<dyn ProxyMonitor>>,
    log_path: Option<PathBuf>,
    closed: Arc<AtomicBool>,
    /// Keep the `TempFile` for the user-pass file in the struct, so it's removed on drop.
    _user_pass_file: mktemp::TempFile,
    /// Keep the 'TempFile' for the proxy user-pass file in the struct, so it's removed on drop.
    _proxy_auth_file: Option<mktemp::TempFile>,

    runtime: tokio::runtime::Runtime,
    event_server_abort_tx: triggered::Trigger,
    server_join_handle: Option<task::JoinHandle<std::result::Result<(), event_server::Error>>>,
}

impl OpenVpnMonitor<OpenVpnCommand> {
    /// Creates a new `OpenVpnMonitor` with the given listener and using the plugin at the given
    /// path.
    pub fn start<L>(
        on_event: L,
        params: &openvpn::TunnelParameters,
        log_path: Option<PathBuf>,
        resource_dir: &Path,
    ) -> Result<Self>
    where
        L: Fn(TunnelEvent) + Send + Sync + 'static,
    {
        let user_pass_file =
            Self::create_credentials_file(&params.config.username, &params.config.password)
                .map_err(Error::CredentialsWriteError)?;

        let proxy_auth_file =
            Self::create_proxy_auth_file(&params.proxy).map_err(Error::CredentialsWriteError)?;

        let user_pass_file_path = user_pass_file.to_path_buf();

        let proxy_auth_file_path = match proxy_auth_file {
            Some(ref file) => Some(file.to_path_buf()),
            _ => None,
        };

        let on_openvpn_event = move |event, env| {
            if event == openvpn_plugin::EventType::RouteUp {
                // The user-pass file has been read. Try to delete it early.
                let _ = fs::remove_file(&user_pass_file_path);

                // The proxy auth file has been read. Try to delete it early.
                if let Some(ref file_path) = &proxy_auth_file_path {
                    let _ = fs::remove_file(file_path);
                }
            }
            match TunnelEvent::from_openvpn_event(event, &env) {
                Some(tunnel_event) => on_event(tunnel_event),
                None => log::debug!("Ignoring OpenVpnEvent {:?}", event),
            }
        };

        let log_dir: Option<PathBuf> = if let Some(ref log_path) = log_path {
            Some(log_path.parent().expect("log_path has no parent").into())
        } else {
            None
        };

        let proxy_resources = proxy::ProxyResourceData {
            resource_dir: resource_dir.to_path_buf(),
            log_dir,
        };

        let proxy_monitor = Self::start_proxy(&params.proxy, &proxy_resources)?;

        let cmd = Self::create_openvpn_cmd(
            params,
            user_pass_file.as_ref(),
            match proxy_auth_file {
                Some(ref file) => Some(file.as_ref()),
                _ => None,
            },
            resource_dir,
            &proxy_monitor,
        )?;

        let plugin_path = Self::get_plugin_path(resource_dir)?;

        Self::new_internal(
            cmd,
            on_openvpn_event,
            &plugin_path,
            log_path,
            user_pass_file,
            proxy_auth_file,
            proxy_monitor,
        )
    }
}

impl<C: OpenVpnBuilder + 'static> OpenVpnMonitor<C> {
    fn new_internal<L>(
        mut cmd: C,
        on_event: L,
        plugin_path: impl AsRef<Path>,
        log_path: Option<PathBuf>,
        user_pass_file: mktemp::TempFile,
        proxy_auth_file: Option<mktemp::TempFile>,
        proxy_monitor: Option<Box<dyn ProxyMonitor>>,
    ) -> Result<OpenVpnMonitor<C>>
    where
        L: Fn(openvpn_plugin::EventType, HashMap<String, String>) + Send + Sync + 'static,
    {
        let uuid = uuid::Uuid::new_v4().to_string();
        let ipc_path = if cfg!(windows) {
            format!("//./pipe/talpid-openvpn-{}", uuid)
        } else {
            format!("/tmp/talpid-openvpn-{}", uuid)
        };

        let (event_server_abort_tx, event_server_abort_rx) = triggered::trigger();

        let mut runtime = tokio::runtime::Builder::new()
            .threaded_scheduler()
            .core_threads(1)
            .enable_all()
            .build()
            .map_err(Error::RuntimeError)?;

        let (start_tx, start_rx) = mpsc::channel();
        let server_join_handle = runtime.spawn(event_server::start(
            ipc_path.clone(),
            start_tx,
            on_event,
            event_server_abort_rx,
        ));
        if let Err(_) = start_rx.recv() {
            return Err(runtime
                .block_on(server_join_handle)
                .expect("Failed to resolve quit handle future")
                .map_err(Error::EventDispatcherError)
                .unwrap_err());
        }

        let child = cmd
            .plugin(plugin_path, vec![ipc_path])
            .log(log_path.as_ref().map(|p| p.as_path()))
            .start()
            .map_err(|e| Error::ChildProcessError("Failed to start", e))?;

        Ok(OpenVpnMonitor {
            child: Arc::new(child),
            proxy_monitor,
            log_path,
            closed: Arc::new(AtomicBool::new(false)),
            _user_pass_file: user_pass_file,
            _proxy_auth_file: proxy_auth_file,

            runtime,
            event_server_abort_tx,
            server_join_handle: Some(server_join_handle),
        })
    }

    /// Creates a handle to this monitor, allowing the tunnel to be closed while some other
    /// thread is blocked in `wait`.
    pub fn close_handle(&self) -> OpenVpnCloseHandle<C::ProcessHandle> {
        OpenVpnCloseHandle {
            child: self.child.clone(),
            closed: self.closed.clone(),
        }
    }

    /// Consumes the monitor and waits for both proxy and tunnel, as applicable.
    pub fn wait(mut self) -> Result<()> {
        if let Some(mut proxy_monitor) = self.proxy_monitor.take() {
            let (tx_tunnel, rx) = mpsc::channel();
            let tx_proxy = tx_tunnel.clone();
            let tunnel_close_handle = self.close_handle();
            let proxy_close_handle = proxy_monitor.close_handle();

            enum Stopped {
                Tunnel(Result<()>),
                Proxy(proxy::Result<proxy::WaitResult>),
            }

            thread::spawn(move || {
                tx_tunnel.send(Stopped::Tunnel(self.wait_tunnel())).unwrap();
                let _ = proxy_close_handle.close();
            });

            thread::spawn(move || {
                tx_proxy.send(Stopped::Proxy(proxy_monitor.wait())).unwrap();
                let _ = tunnel_close_handle.close();
            });

            let result = rx.recv().expect("wait got no result");
            let _ = rx.recv();

            match result {
                Stopped::Tunnel(tunnel_result) => tunnel_result,
                Stopped::Proxy(proxy_result) => {
                    // The proxy should never exit before openvpn.
                    match proxy_result {
                        Ok(proxy::WaitResult::ProperShutdown) => {
                            Err(Error::ProxyExited("No details".to_owned()))
                        }
                        Ok(proxy::WaitResult::UnexpectedExit(details)) => {
                            Err(Error::ProxyExited(details))
                        }
                        Err(err) => Err(err).map_err(Error::MonitorProxyError),
                    }
                }
            }
        } else {
            // No proxy active, wait only for the tunnel.
            self.wait_tunnel()
        }
    }

    /// Supplement `inner_wait_tunnel()` with logging and error handling.
    fn wait_tunnel(&mut self) -> Result<()> {
        let result = self.inner_wait_tunnel();
        match result {
            WaitResult::Child(Ok(exit_status), closed) => {
                if exit_status.success() || closed {
                    log::debug!(
                        "OpenVPN exited, as expected, with exit status: {}",
                        exit_status
                    );
                    Ok(())
                } else {
                    log::error!("OpenVPN died unexpectedly with status: {}", exit_status);
                    Err(self.postmortem())
                }
            }
            WaitResult::Child(Err(e), _) => {
                log::error!("OpenVPN process wait error: {}", e);
                Err(Error::ChildProcessError("Error when waiting", e))
            }
            WaitResult::EventDispatcher => {
                log::error!("OpenVPN Event server exited unexpectedly");
                Err(Error::EventDispatcherExited)
            }
        }
    }

    /// Waits for both the child process and the event dispatcher in parallel. After both have
    /// returned this returns the earliest result.
    fn inner_wait_tunnel(&mut self) -> WaitResult {
        let child_wait_handle = self.child.clone();
        let closed_handle = self.closed.clone();
        let child_close_handle = self.close_handle();

        let (child_tx, rx) = mpsc::channel();
        let dispatcher_tx = child_tx.clone();

        let event_server_abort_tx = self.event_server_abort_tx.clone();

        thread::spawn(move || {
            let result = child_wait_handle.wait();
            let closed = closed_handle.load(Ordering::SeqCst);
            child_tx.send(WaitResult::Child(result, closed)).unwrap();
            event_server_abort_tx.trigger();
        });

        let server_join_handle = self
            .server_join_handle
            .take()
            .expect("No event server quit handle");
        self.runtime.spawn(async move {
            let _ = server_join_handle.await;
            dispatcher_tx.send(WaitResult::EventDispatcher).unwrap();
            let _ = child_close_handle.close();
        });

        let result = rx.recv().expect("inner_wait_tunnel no result");
        let _ = rx.recv().expect("inner_wait_tunnel no second result");
        result
    }

    /// Performs a postmortem analysis to attempt to provide a more detailed error result.
    fn postmortem(&mut self) -> Error {
        #[cfg(windows)]
        {
            if let Some(log_path) = self.log_path.take() {
                if let Ok(log) = fs::read_to_string(log_path) {
                    if log.contains("There are no TAP-Windows adapters on this system") {
                        return Error::MissingTapAdapter;
                    }
                    if log.contains("CreateFile failed on TAP device") {
                        return Error::DisabledTapAdapter;
                    }
                }
            }
        }

        Error::ChildProcessDied
    }

    fn create_proxy_auth_file(
        proxy_settings: &Option<openvpn::ProxySettings>,
    ) -> std::result::Result<Option<mktemp::TempFile>, io::Error> {
        if let Some(openvpn::ProxySettings::Remote(ref remote_proxy)) = proxy_settings {
            if let Some(ref proxy_auth) = remote_proxy.auth {
                return Ok(Some(Self::create_credentials_file(
                    &proxy_auth.username,
                    &proxy_auth.password,
                )?));
            }
        }
        Ok(None)
    }

    /// Starts a proxy service, as applicable.
    fn start_proxy(
        proxy_settings: &Option<openvpn::ProxySettings>,
        proxy_resources: &ProxyResourceData,
    ) -> Result<Option<Box<dyn ProxyMonitor>>> {
        if let Some(ref settings) = proxy_settings {
            let proxy_monitor =
                proxy::start_proxy(settings, proxy_resources).map_err(Error::StartProxyError)?;
            return Ok(Some(proxy_monitor));
        }
        Ok(None)
    }

    fn create_credentials_file(username: &str, password: &str) -> io::Result<mktemp::TempFile> {
        let temp_file = mktemp::TempFile::new();
        log::debug!("Writing credentials to {}", temp_file.as_ref().display());
        let mut file = fs::File::create(&temp_file)?;
        Self::set_user_pass_file_permissions(&file)?;
        write!(file, "{}\n{}\n", username, password)?;
        Ok(temp_file)
    }


    #[cfg(unix)]
    fn set_user_pass_file_permissions(file: &fs::File) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(PermissionsExt::from_mode(0o400))
    }

    #[cfg(windows)]
    fn set_user_pass_file_permissions(_file: &fs::File) -> io::Result<()> {
        // TODO(linus): Lock permissions correctly on Windows.
        Ok(())
    }

    fn get_plugin_path(resource_dir: &Path) -> Result<PathBuf> {
        let path = resource_dir.join(OPENVPN_PLUGIN_FILENAME);
        if path.exists() {
            log::trace!("Using OpenVPN plugin at {}", path.display());
            Ok(path)
        } else {
            Err(Error::PluginNotFound(path.display().to_string()))
        }
    }

    fn create_openvpn_cmd(
        params: &openvpn::TunnelParameters,
        user_pass_file: &Path,
        proxy_auth_file: Option<&Path>,
        resource_dir: &Path,
        proxy_monitor: &Option<Box<dyn ProxyMonitor>>,
    ) -> Result<OpenVpnCommand> {
        let mut cmd = OpenVpnCommand::new(Self::get_openvpn_bin(resource_dir)?);
        if let Some(config) = Self::get_config_path(resource_dir) {
            cmd.config(config);
        }
        #[cfg(target_os = "linux")]
        cmd.iproute_bin(which::which("ip").map_err(Error::IpRouteNotFound)?);
        cmd.remote(params.config.endpoint)
            .user_pass(user_pass_file)
            .tunnel_options(&params.options)
            .enable_ipv6(params.generic_options.enable_ipv6)
            .ca(resource_dir.join("ca.crt"));
        #[cfg(windows)]
        cmd.tunnel_alias(Some(
            crate::winnet::get_tap_interface_alias().map_err(Error::WinnetError)?,
        ));
        if let Some(proxy_settings) = params.proxy.clone().take() {
            cmd.proxy_settings(proxy_settings);
        }
        if let Some(proxy_auth_file) = proxy_auth_file {
            cmd.proxy_auth(proxy_auth_file);
        }
        if let Some(proxy) = proxy_monitor {
            cmd.proxy_port(proxy.port());
        }

        Ok(cmd)
    }

    fn get_openvpn_bin(resource_dir: &Path) -> Result<PathBuf> {
        let path = resource_dir.join(OPENVPN_BIN_FILENAME);
        if path.exists() {
            log::trace!("Using OpenVPN at {}", path.display());
            Ok(path)
        } else {
            Err(Error::OpenVpnNotFound(path.display().to_string()))
        }
    }

    fn get_config_path(resource_dir: &Path) -> Option<PathBuf> {
        let path = resource_dir.join("openvpn.conf");
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}

/// A handle to an `OpenVpnMonitor` for closing it.
#[derive(Debug, Clone)]
pub struct OpenVpnCloseHandle<H: ProcessHandle = OpenVpnProcHandle> {
    child: Arc<H>,
    closed: Arc<AtomicBool>,
}

impl<H: ProcessHandle> OpenVpnCloseHandle<H> {
    /// Kills the underlying OpenVPN process, making the `OpenVpnMonitor::wait` method return.
    pub fn close(self) -> io::Result<()> {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.child.kill()
        } else {
            Ok(())
        }
    }
}

/// Internal enum to differentiate between if the child process or the event dispatcher died first.
#[derive(Debug)]
enum WaitResult {
    Child(io::Result<ExitStatus>, bool),
    EventDispatcher,
}

/// Trait for types acting as OpenVPN process starters for `OpenVpnMonitor`.
pub trait OpenVpnBuilder {
    /// The type of handles to subprocesses this builder produces.
    type ProcessHandle: ProcessHandle;

    /// Set the OpenVPN plugin to the given values.
    fn plugin(&mut self, path: impl AsRef<Path>, args: Vec<String>) -> &mut Self;

    /// Set the OpenVPN log file path to use.
    fn log(&mut self, log_path: Option<impl AsRef<Path>>) -> &mut Self;

    /// Spawn the subprocess and return a handle.
    fn start(&self) -> io::Result<Self::ProcessHandle>;
}

/// Trait for types acting as handles to subprocesses for `OpenVpnMonitor`
pub trait ProcessHandle: Send + Sync + 'static {
    /// Block until the subprocess exits or there is an error in the wait syscall.
    fn wait(&self) -> io::Result<ExitStatus>;

    /// Kill the subprocess.
    fn kill(&self) -> io::Result<()>;
}

impl OpenVpnBuilder for OpenVpnCommand {
    type ProcessHandle = OpenVpnProcHandle;

    fn plugin(&mut self, path: impl AsRef<Path>, args: Vec<String>) -> &mut Self {
        self.plugin(path, args)
    }

    fn log(&mut self, log_path: Option<impl AsRef<Path>>) -> &mut Self {
        if let Some(log_path) = log_path {
            self.log(log_path)
        } else {
            self
        }
    }

    fn start(&self) -> io::Result<OpenVpnProcHandle> {
        OpenVpnProcHandle::new(self.build())
    }
}

impl ProcessHandle for OpenVpnProcHandle {
    fn wait(&self) -> io::Result<ExitStatus> {
        self.inner.wait().map(|output| output.status)
    }

    fn kill(&self) -> io::Result<()> {
        self.nice_kill(OPENVPN_DIE_TIMEOUT)
    }
}


mod event_server {
    use futures::stream::TryStreamExt;
    use parity_tokio_ipc::{Endpoint as IpcEndpoint, SecurityAttributes};
    use std::{
        collections::HashMap,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, AsyncWrite};
    use tonic::{
        self,
        transport::{server::Connected, Server},
        Request, Response,
    };

    mod proto {
        tonic::include_proto!("talpid_openvpn_plugin");
    }
    use proto::{
        openvpn_event_proxy_server::{OpenvpnEventProxy, OpenvpnEventProxyServer},
        EventType,
    };

    #[derive(err_derive::Error, Debug)]
    pub enum Error {
        /// Failure to set up the IPC server.
        #[error(display = "Failed to create pipe or Unix socket")]
        StartServer(#[error(source)] std::io::Error),

        /// An error occurred while the server was running.
        #[error(display = "Tonic error")]
        TonicError(#[error(source)] tonic::transport::Error),
    }

    /// Implements a gRPC service used to process events sent to by OpenVPN.
    #[derive(Debug)]
    pub struct OpenvpnEventProxyImpl<L> {
        on_event: L,
    }

    #[tonic::async_trait]
    impl<L> OpenvpnEventProxy for OpenvpnEventProxyImpl<L>
    where
        L: Fn(openvpn_plugin::EventType, HashMap<String, String>) + Send + Sync + 'static,
    {
        async fn event(
            &self,
            request: Request<EventType>,
        ) -> std::result::Result<Response<()>, tonic::Status> {
            log::info!("OpenVPN event {:?}", request);

            let request = request.into_inner();

            let event_type = openvpn_plugin::EventType::try_from(request.event)
                .ok_or(tonic::Status::invalid_argument("Unknown event type"))?;

            (self.on_event)(event_type, request.env);

            Ok(Response::new(()))
        }
    }

    pub async fn start<L>(
        ipc_path: String,
        server_start_tx: std::sync::mpsc::Sender<()>,
        on_event: L,
        abort_rx: triggered::Listener,
    ) -> std::result::Result<(), Error>
    where
        L: Fn(openvpn_plugin::EventType, HashMap<String, String>) + Send + Sync + 'static,
    {
        let mut endpoint = IpcEndpoint::new(ipc_path.clone());
        endpoint.set_security_attributes(SecurityAttributes::allow_everyone_create().unwrap());
        let incoming = endpoint.incoming().map_err(Error::StartServer)?;
        let _ = server_start_tx.send(());

        let server = OpenvpnEventProxyImpl { on_event };

        Server::builder()
            .add_service(OpenvpnEventProxyServer::new(server))
            .serve_with_incoming_shutdown(incoming.map_ok(StreamBox), abort_rx)
            .await
            .map_err(Error::TonicError)
    }

    #[derive(Debug)]
    pub struct StreamBox<T: AsyncRead + AsyncWrite>(pub T);
    impl<T: AsyncRead + AsyncWrite> Connected for StreamBox<T> {}
    impl<T: AsyncRead + AsyncWrite + Unpin> AsyncRead for StreamBox<T> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }
    impl<T: AsyncRead + AsyncWrite + Unpin> AsyncWrite for StreamBox<T> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::mktemp::TempFile;
    use parking_lot::Mutex;
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    #[derive(Debug, Default, Clone)]
    struct TestOpenVpnBuilder {
        pub plugin: Arc<Mutex<Option<PathBuf>>>,
        pub log: Arc<Mutex<Option<PathBuf>>>,
        pub process_handle: Option<TestProcessHandle>,
    }

    impl OpenVpnBuilder for TestOpenVpnBuilder {
        type ProcessHandle = TestProcessHandle;

        fn plugin(&mut self, path: impl AsRef<Path>, _args: Vec<String>) -> &mut Self {
            *self.plugin.lock() = Some(path.as_ref().to_path_buf());
            self
        }

        fn log(&mut self, log: Option<impl AsRef<Path>>) -> &mut Self {
            *self.log.lock() = log.as_ref().map(|path| path.as_ref().to_path_buf());
            self
        }

        fn start(&self) -> io::Result<Self::ProcessHandle> {
            self.process_handle
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "failed to start"))
        }
    }

    #[derive(Debug, Copy, Clone)]
    struct TestProcessHandle(i32);

    impl ProcessHandle for TestProcessHandle {
        #[cfg(unix)]
        fn wait(&self) -> io::Result<ExitStatus> {
            use std::os::unix::process::ExitStatusExt;
            Ok(ExitStatus::from_raw(self.0))
        }

        #[cfg(windows)]
        fn wait(&self) -> io::Result<ExitStatus> {
            use std::os::windows::process::ExitStatusExt;
            Ok(ExitStatus::from_raw(self.0 as u32))
        }

        fn kill(&self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn sets_plugin() {
        let builder = TestOpenVpnBuilder::default();
        let _ = OpenVpnMonitor::new_internal(
            builder.clone(),
            |_, _| {},
            "./my_test_plugin",
            None,
            TempFile::new(),
            None,
            None,
        );
        assert_eq!(
            Some(PathBuf::from("./my_test_plugin")),
            *builder.plugin.lock()
        );
    }

    #[test]
    fn sets_log() {
        let builder = TestOpenVpnBuilder::default();
        let _ = OpenVpnMonitor::new_internal(
            builder.clone(),
            |_, _| {},
            "",
            Some(PathBuf::from("./my_test_log_file")),
            TempFile::new(),
            None,
            None,
        );
        assert_eq!(
            Some(PathBuf::from("./my_test_log_file")),
            *builder.log.lock()
        );
    }

    #[test]
    fn exit_successfully() {
        let mut builder = TestOpenVpnBuilder::default();
        builder.process_handle = Some(TestProcessHandle(0));
        let testee =
            OpenVpnMonitor::new_internal(builder, |_, _| {}, "", None, TempFile::new(), None, None)
                .unwrap();
        assert!(testee.wait().is_ok());
    }

    #[test]
    fn exit_error() {
        let mut builder = TestOpenVpnBuilder::default();
        builder.process_handle = Some(TestProcessHandle(1));
        let testee =
            OpenVpnMonitor::new_internal(builder, |_, _| {}, "", None, TempFile::new(), None, None)
                .unwrap();
        assert!(testee.wait().is_err());
    }

    #[test]
    fn wait_closed() {
        let mut builder = TestOpenVpnBuilder::default();
        builder.process_handle = Some(TestProcessHandle(1));
        let testee =
            OpenVpnMonitor::new_internal(builder, |_, _| {}, "", None, TempFile::new(), None, None)
                .unwrap();
        testee.close_handle().close().unwrap();
        assert!(testee.wait().is_ok());
    }

    #[test]
    fn failed_process_start() {
        let builder = TestOpenVpnBuilder::default();
        let error =
            OpenVpnMonitor::new_internal(builder, |_, _| {}, "", None, TempFile::new(), None, None)
                .unwrap_err();
        match error {
            Error::ChildProcessError(..) => (),
            _ => panic!("Wrong error"),
        }
    }
}
