/// A NOOP struct for bringing the API between Windows and Unix up to parity. To set permissions
/// properly on Unix, you can just use `std::os::unix::fs::PermissionsExt`.
pub struct SecurityAttributes {}

impl SecurityAttributes {
    pub fn empty() -> Self {Self{}}
    pub fn allow_everyone_connect() -> Self {Self{}}
    pub fn allow_everyone_create() -> Self {Self{}}
}
