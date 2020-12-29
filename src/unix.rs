use libc::chmod;
use std::ffi::CString;
use std::io::{self, Error};
use futures::Stream;
use tokio::prelude::*;
use tokio::net::{UnixListener, UnixStream};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::mem::MaybeUninit;

/// Socket permissions and ownership on UNIX
pub struct SecurityAttributes {
    // read/write permissions for owner, group and others in unix octal.
    mode: Option<u16>
}

impl SecurityAttributes {
    /// New default security attributes. These only allow access by the
    /// process’s own user and the system administrator.
    pub fn empty() -> Self {
        SecurityAttributes {
            mode: Some(0o600)
        }
    }

    /// New security attributes that allow everyone to connect.
    pub fn allow_everyone_connect(mut self) -> io::Result<Self> {
        self.mode = Some(0o666);
        Ok(self)
    }

    /// Set a custom permission on the socket
    pub fn set_mode(mut self, mode: u16) -> io::Result<Self> {
        self.mode = Some(mode);
        Ok(self)
    }

    /// New security attributes that allow everyone to create.
    ///
    /// This does not work on unix, where it is equivalent to
    /// [`SecurityAttributes::allow_everyone_connect`].
    pub fn allow_everyone_create() -> io::Result<Self> {
        Ok(SecurityAttributes {
            mode: None
        })
    }

    /// called in unix, after server socket has been created
    /// will apply security attributes to the socket.
    fn apply_permissions(&self, path: &str) -> io::Result<()> {
        if let Some(mode) = self.mode {
            let path = CString::new(path)?;
            if unsafe { chmod(path.as_ptr(), mode.into()) } == -1 {
                return Err(Error::last_os_error());
            }
        }

        Ok(())
    }
}

/// Endpoint implementation for unix systems
pub struct Endpoint {
    path: String,
    security_attributes: SecurityAttributes,
}

impl Endpoint {
    /// Stream of incoming connections
    pub fn incoming(self) -> io::Result<impl Stream<Item = tokio::io::Result<impl AsyncRead + AsyncWrite>> + 'static> {
        let listener = self.inner()?;
        // the call to bind in `inner()` creates the file
        // `apply_permission()` will set the file permissions.
        self.security_attributes.apply_permissions(&self.path)?;
        Ok(Incoming {
            path: self.path,
            listener,
        })
    }

    /// Inner platform-dependant state of the endpoint
    fn inner(&self) -> io::Result<UnixListener> {
        UnixListener::bind(&self.path)
    }

    /// Set security attributes for the connection
    pub fn set_security_attributes(&mut self, security_attributes: SecurityAttributes) {
        self.security_attributes = security_attributes;
    }

    /// Make new connection using the provided path and running event pool
    pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Connection> {
        Ok(Connection::wrap(UnixStream::connect(path.as_ref()).await?))
    }

    /// Returns the path of the endpoint.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// New IPC endpoint at the given path
    pub fn new(path: String) -> Self {
        Endpoint {
            path,
            security_attributes: SecurityAttributes::empty(),
        }
    }
}

/// Stream of incoming connections.
///
/// Removes the bound socket file when dropped.
struct Incoming {
    path: String,
    listener: UnixListener,
}

impl Stream for Incoming {
    type Item = io::Result<UnixStream>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = Pin::into_inner(self);
        Pin::new(&mut this.listener).poll_next(cx)
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        use std::fs;
        if let Ok(()) = fs::remove_file(&self.path) {
            log::trace!("Removed socket file at: {}", self.path)
        }
    }
}

/// IPC connection.
pub struct Connection {
    inner: UnixStream,
}

impl Connection {
    fn wrap(stream: UnixStream) -> Self {
        Self { inner: stream }
    }
}

impl AsyncRead for Connection {
    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut [MaybeUninit<u8>]) -> bool {
        self.inner.prepare_uninitialized_buffer(buf)
    }

    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = Pin::into_inner(self);
        Pin::new(&mut this.inner).poll_read(ctx, buf)
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let this = Pin::into_inner(self);
        Pin::new(&mut this.inner).poll_write(ctx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = Pin::into_inner(self);
        Pin::new(&mut this.inner).poll_flush(ctx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = Pin::into_inner(self);
        Pin::new(&mut this.inner).poll_shutdown(ctx)
    }
}
