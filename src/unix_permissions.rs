use std::io;

/// A NOOP struct for bringing the API between Windows and Unix up to parity. To set permissions
/// properly on Unix, you can just use `std::os::unix::fs::PermissionsExt`.
pub struct SecurityAttributes;

impl SecurityAttributes {
    /// New default security attributes.
    pub fn empty() -> Self {
        SecurityAttributes
    }

    /// New security attributes that allow everyone to connect.
    pub fn allow_everyone_connect() -> io::Result<Self> {
        Ok(SecurityAttributes)
    }

    /// New security attributes that allow everyone to create.
    pub fn allow_everyone_create() -> io::Result<Self> {
        Ok(SecurityAttributes)
    }
}
