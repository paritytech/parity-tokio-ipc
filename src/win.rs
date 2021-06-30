use winapi::shared::winerror::{ERROR_PIPE_BUSY, ERROR_SUCCESS};
use winapi::um::accctrl::*;
use winapi::um::aclapi::*;
use winapi::um::minwinbase::{LPTR, PSECURITY_ATTRIBUTES, SECURITY_ATTRIBUTES};
use winapi::um::securitybaseapi::*;
use winapi::um::winbase::{LocalAlloc, LocalFree};
use winapi::um::winnt::*;

use futures::Stream;
use std::io;
use std::marker;
use std::mem;
use std::path::Path;
use std::pin::Pin;
use std::ptr;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};

use tokio::net::windows::named_pipe;

enum NamedPipe {
    Server(named_pipe::NamedPipeServer),
    Client(named_pipe::NamedPipeClient),
}

const PIPE_AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(5);

/// Endpoint implementation for windows
pub struct Endpoint {
    path: String,
    security_attributes: SecurityAttributes,
    created_listener: bool,
}

impl Endpoint {
    /// Stream of incoming connections
    pub fn incoming(
        mut self,
    ) -> io::Result<impl Stream<Item = io::Result<impl AsyncRead + AsyncWrite>> + 'static> {
        let pipe = self.create_listener()?;

        let stream =
            futures::stream::try_unfold((pipe, self), |(listener, mut endpoint)| async move {
                let () = listener.connect().await?;

                let new_listener = endpoint.create_listener()?;

                let conn = Connection::wrap(NamedPipe::Server(listener));

                Ok(Some((conn, (new_listener, endpoint))))
            });

        Ok(stream)
    }

    fn create_listener(&mut self) -> io::Result<named_pipe::NamedPipeServer> {
        let server = unsafe {
            named_pipe::ServerOptions::new()
                .first_pipe_instance(!self.created_listener)
                .reject_remote_clients(true)
                .access_inbound(true)
                .access_outbound(true)
                .in_buffer_size(65536)
                .out_buffer_size(65536)
                .create_with_security_attributes_raw(
                    &self.path,
                    self.security_attributes.as_ptr() as *mut libc::c_void,
                )
        }?;
        self.created_listener = true;

        Ok(server)
    }

    /// Set security attributes for the connection
    pub fn set_security_attributes(&mut self, security_attributes: SecurityAttributes) {
        self.security_attributes = security_attributes;
    }

    /// Returns the path of the endpoint.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Make new connection using the provided path and running event pool.
    pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Connection> {
        let path = path.as_ref();

        // There is not async equivalent of waiting for a named pipe in Windows,
        // so we keep trying or sleeping for a bit, until we hit a timeout
        let attempt_start = Instant::now();
        let client = loop {
            match named_pipe::ClientOptions::new()
                .read(true)
                .write(true)
                .open(path)
            {
                Ok(client) => break client,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    if attempt_start.elapsed() < PIPE_AVAILABILITY_TIMEOUT {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        };

        Ok(Connection::wrap(NamedPipe::Client(client)))
    }

    /// New IPC endpoint at the given path
    pub fn new(path: String) -> Self {
        Endpoint {
            path,
            security_attributes: SecurityAttributes::empty(),
            created_listener: false,
        }
    }
}

/// IPC connection.
pub struct Connection {
    inner: NamedPipe,
}

impl Connection {
    /// Wraps an existing named pipe
    fn wrap(pipe: NamedPipe) -> Self {
        Self { inner: pipe }
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        match this.inner {
            NamedPipe::Client(ref mut c) => Pin::new(c).poll_read(ctx, buf),
            NamedPipe::Server(ref mut s) => Pin::new(s).poll_read(ctx, buf),
        }
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let this = Pin::into_inner(self);
        match this.inner {
            NamedPipe::Client(ref mut c) => Pin::new(c).poll_write(ctx, buf),
            NamedPipe::Server(ref mut s) => Pin::new(s).poll_write(ctx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = Pin::into_inner(self);
        match this.inner {
            NamedPipe::Client(ref mut c) => Pin::new(c).poll_flush(ctx),
            NamedPipe::Server(ref mut s) => Pin::new(s).poll_flush(ctx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = Pin::into_inner(self);
        match this.inner {
            NamedPipe::Client(ref mut c) => Pin::new(c).poll_shutdown(ctx),
            NamedPipe::Server(ref mut s) => Pin::new(s).poll_shutdown(ctx),
        }
    }
}

/// Security attributes.
pub struct SecurityAttributes {
    attributes: Option<InnerAttributes>,
}

pub const DEFAULT_SECURITY_ATTRIBUTES: SecurityAttributes = SecurityAttributes {
    attributes: Some(InnerAttributes {
        descriptor: SecurityDescriptor {
            descriptor_ptr: ptr::null_mut(),
        },
        acl: Acl {
            acl_ptr: ptr::null_mut(),
        },
        attrs: SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: 0,
        },
    }),
};

impl SecurityAttributes {
    /// New default security attributes.
    pub fn empty() -> SecurityAttributes {
        DEFAULT_SECURITY_ATTRIBUTES
    }

    /// New default security attributes that allow everyone to connect.
    pub fn allow_everyone_connect(&self) -> io::Result<SecurityAttributes> {
        let attributes = Some(InnerAttributes::allow_everyone(
            GENERIC_READ | FILE_WRITE_DATA,
        )?);
        Ok(SecurityAttributes { attributes })
    }

    /// Set a custom permission on the socket
    pub fn set_mode(self, _mode: u32) -> io::Result<Self> {
        // for now, does nothing.
        Ok(self)
    }

    /// New default security attributes that allow everyone to create.
    pub fn allow_everyone_create() -> io::Result<SecurityAttributes> {
        let attributes = Some(InnerAttributes::allow_everyone(
            GENERIC_READ | GENERIC_WRITE,
        )?);
        Ok(SecurityAttributes { attributes })
    }

    /// Return raw handle of security attributes.
    pub(crate) unsafe fn as_ptr(&mut self) -> PSECURITY_ATTRIBUTES {
        match self.attributes.as_mut() {
            Some(attributes) => attributes.as_ptr(),
            None => ptr::null_mut(),
        }
    }
}

unsafe impl Send for SecurityAttributes {}

struct Sid {
    sid_ptr: PSID,
}

impl Sid {
    fn everyone_sid() -> io::Result<Sid> {
        let mut sid_ptr = ptr::null_mut();
        let result = unsafe {
            #[allow(const_item_mutation)]
            AllocateAndInitializeSid(
                SECURITY_WORLD_SID_AUTHORITY.as_mut_ptr() as *mut _,
                1,
                SECURITY_WORLD_RID,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                &mut sid_ptr,
            )
        };
        if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Sid { sid_ptr })
        }
    }

    // Unsafe - the returned pointer is only valid for the lifetime of self.
    unsafe fn as_ptr(&self) -> PSID {
        self.sid_ptr
    }
}

impl Drop for Sid {
    fn drop(&mut self) {
        if !self.sid_ptr.is_null() {
            unsafe {
                FreeSid(self.sid_ptr);
            }
        }
    }
}

struct AceWithSid<'a> {
    explicit_access: EXPLICIT_ACCESS_W,
    _marker: marker::PhantomData<&'a Sid>,
}

impl<'a> AceWithSid<'a> {
    fn new(sid: &'a Sid, trustee_type: u32) -> AceWithSid<'a> {
        let mut explicit_access = unsafe { mem::zeroed::<EXPLICIT_ACCESS_W>() };
        explicit_access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
        explicit_access.Trustee.TrusteeType = trustee_type;
        explicit_access.Trustee.ptstrName = unsafe { sid.as_ptr() as *mut _ };

        AceWithSid {
            explicit_access,
            _marker: marker::PhantomData,
        }
    }

    fn set_access_mode(&mut self, access_mode: u32) -> &mut Self {
        self.explicit_access.grfAccessMode = access_mode;
        self
    }

    fn set_access_permissions(&mut self, access_permissions: u32) -> &mut Self {
        self.explicit_access.grfAccessPermissions = access_permissions;
        self
    }

    fn allow_inheritance(&mut self, inheritance_flags: u32) -> &mut Self {
        self.explicit_access.grfInheritance = inheritance_flags;
        self
    }
}

struct Acl {
    acl_ptr: PACL,
}

impl Acl {
    fn empty() -> io::Result<Acl> {
        Self::new(&mut [])
    }

    fn new(entries: &mut [AceWithSid<'_>]) -> io::Result<Acl> {
        let mut acl_ptr = ptr::null_mut();
        let result = unsafe {
            SetEntriesInAclW(
                entries.len() as u32,
                entries.as_mut_ptr() as *mut _,
                ptr::null_mut(),
                &mut acl_ptr,
            )
        };

        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }

        Ok(Acl { acl_ptr })
    }

    unsafe fn as_ptr(&self) -> PACL {
        self.acl_ptr
    }
}

impl Drop for Acl {
    fn drop(&mut self) {
        if !self.acl_ptr.is_null() {
            unsafe { LocalFree(self.acl_ptr as *mut _) };
        }
    }
}

struct SecurityDescriptor {
    descriptor_ptr: PSECURITY_DESCRIPTOR,
}

impl SecurityDescriptor {
    fn new() -> io::Result<Self> {
        let descriptor_ptr = unsafe { LocalAlloc(LPTR, SECURITY_DESCRIPTOR_MIN_LENGTH) };
        if descriptor_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Failed to allocate security descriptor",
            ));
        }

        if unsafe {
            InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) == 0
        } {
            return Err(io::Error::last_os_error());
        };

        Ok(SecurityDescriptor { descriptor_ptr })
    }

    fn set_dacl(&mut self, acl: &Acl) -> io::Result<()> {
        if unsafe {
            SetSecurityDescriptorDacl(self.descriptor_ptr, true as i32, acl.as_ptr(), false as i32)
                == 0
        } {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    unsafe fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.descriptor_ptr
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.descriptor_ptr.is_null() {
            unsafe { LocalFree(self.descriptor_ptr) };
            self.descriptor_ptr = ptr::null_mut();
        }
    }
}

struct InnerAttributes {
    descriptor: SecurityDescriptor,
    acl: Acl,
    attrs: SECURITY_ATTRIBUTES,
}

impl InnerAttributes {
    fn empty() -> io::Result<InnerAttributes> {
        let descriptor = SecurityDescriptor::new()?;
        let mut attrs = unsafe { mem::zeroed::<SECURITY_ATTRIBUTES>() };
        attrs.nLength = mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
        attrs.lpSecurityDescriptor = unsafe { descriptor.as_ptr() };
        attrs.bInheritHandle = false as i32;

        let acl = Acl::empty().expect("this should never fail");

        Ok(InnerAttributes {
            acl,
            descriptor,
            attrs,
        })
    }

    fn allow_everyone(permissions: u32) -> io::Result<InnerAttributes> {
        let mut attributes = Self::empty()?;
        let sid = Sid::everyone_sid()?;

        let mut everyone_ace = AceWithSid::new(&sid, TRUSTEE_IS_WELL_KNOWN_GROUP);
        everyone_ace
            .set_access_mode(SET_ACCESS)
            .set_access_permissions(permissions)
            .allow_inheritance(false as u32);

        let mut entries = vec![everyone_ace];
        attributes.acl = Acl::new(&mut entries)?;
        attributes.descriptor.set_dacl(&attributes.acl)?;

        Ok(attributes)
    }

    unsafe fn as_ptr(&mut self) -> PSECURITY_ATTRIBUTES {
        &mut self.attrs as *mut _
    }
}

#[cfg(test)]
mod test {
    use super::SecurityAttributes;

    #[test]
    fn test_allow_everyone_everything() {
        SecurityAttributes::allow_everyone_create()
            .expect("failed to create security attributes that allow everyone to create a pipe");
    }

    #[test]
    fn test_allow_eveyone_read_write() {
        SecurityAttributes::empty()
            .allow_everyone_connect()
            .expect("failed to create security attributes that allow everyone to read and write to/from a pipe");
    }
}
