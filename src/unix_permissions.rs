use std::io;
use libc::chmod;
use std::ffi::CString;
use std::io::Error;

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
