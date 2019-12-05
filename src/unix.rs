use std::io;
use libc::chmod;
use std::ffi::CString;
use std::io::Error;
use futures::Stream;
use tokio::prelude::*;
use tokio::net::{UnixListener};
use std::path::Path;

/// Socket permissions and ownership on UNIX
pub struct SecurityAttributes {
    // read/write permissions for owner, group and others in unix octal.
    mode: Option<u16>
}

impl SecurityAttributes {
    /// New default security attributes.
    pub fn empty() -> Self {
        SecurityAttributes {
            mode: None
        }
    }

    /// New security attributes that allow everyone to connect.
    pub fn allow_everyone_connect(mut self) -> io::Result<Self> {
        self.mode = Some(0o777);
        Ok(self)
    }

    /// Set a custom permission on the socket
    pub fn set_mode(mut self, mode: u16) -> io::Result<Self> {
        self.mode = Some(mode);
        Ok(self)
    }

    /// New security attributes that allow everyone to create.
    pub fn allow_everyone_create() -> io::Result<Self> {
        Ok(SecurityAttributes {
            mode: None
        })
    }

    /// called in unix, after server socket has been created
    /// will apply security attributes to the socket.
     pub(crate) unsafe fn apply_permissions(&self, path: &str) -> io::Result<()> {
        let path = CString::new(path.to_owned())?;
         if let Some(mode) = self.mode {
            if chmod(path.as_ptr(), mode as _) == -1 {
                return Err(Error::last_os_error())
            }
        }

        Ok(())
    }
}

/// Endpoint implementation for unix systems
pub struct Endpoint {
    path: String,
    security_attributes: SecurityAttributes,
    unix_listener: Option<UnixListener>,
}

impl Endpoint {
    /// Stream of incoming connections
    pub fn incoming(&mut self) -> io::Result<impl Stream<Item = tokio::io::Result<impl AsyncRead + AsyncWrite>> + '_> {
        self.unix_listener = Some(self.inner()?);
        unsafe {
            // the call to bind in `inner()` creates the file
            // `apply_permission()` will set the file permissions.
            self.security_attributes.apply_permissions(&self.path)?;
        };
        // for some unknown reason, the Incoming struct borrows the listener
        // so we have to hold on to the listener in order to return the Incoming struct.
        Ok(self.unix_listener.as_mut().unwrap().incoming())
    }

    /// Inner platform-dependant state of the endpoint
    fn inner(&self) -> io::Result<UnixListener> {
        UnixListener::bind(&self.path)
    }

    /// Set security attributes for the connection
    pub fn set_security_attributes(&mut self, security_attributes: SecurityAttributes) {
        self.security_attributes = security_attributes;
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
            unix_listener: None,
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        use std::fs;
        if let Ok(()) = fs::remove_file(Path::new(&self.path)) {
            log::trace!("Removed socket file at: {}", self.path)
        }
    }
}
