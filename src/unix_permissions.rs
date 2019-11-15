use std::io;
use libc::chmod;
use std::ffi::CString;
use std::io::Error;

/// A NOOP struct for bringing the API between Windows and Unix up to parity. To set permissions
/// properly on Unix, you can just use `std::os::unix::fs::PermissionsExt`.
pub struct SecurityAttributes {
    mode: Option<u32>
}

impl SecurityAttributes {
    /// New default security attributes.
    pub fn empty() -> Self {
        SecurityAttributes {
            mode: None
        }
    }

    /// New security attributes that allow everyone to connect.
    pub fn allow_everyone_connect(mut self, mode: Option<u32>) -> io::Result<Self> {
        self.mode = mode;
        Ok(self)
    }

    /// New security attributes that allow everyone to create.
    pub fn allow_everyone_create() -> io::Result<Self> {
        Ok(SecurityAttributes {
            mode: None
        })
    }

     pub(crate) unsafe fn apply_permissions(&self, path: &str) -> io::Result<()> {
        let path = CString::new(path.to_owned())?;
         if let Some(mode) = self.mode {
            if chmod(path.as_ptr(), mode) == -1 {
                return Err(Error::last_os_error())
            }
        }

        Ok(())
    }
}
