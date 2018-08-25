//! Tokio IPC transport. Under the hood uses Unix Domain Sockets for Linux/Mac 
//! and Named Pipes for Windows. 

extern crate futures;
extern crate tokio_uds;
extern crate tokio_named_pipes;
extern crate tokio_core;

extern crate tokio_io;
extern crate bytes;
#[allow(unused_imports)] #[macro_use] extern crate log;

#[cfg(windows)]
extern crate miow;
#[cfg(windows)]
extern crate mio_named_pipes;
#[cfg(windows)]
extern crate winapi;

use std::io::{self, Read, Write};
use std::path::Path;

use futures::{Async, Poll};
use futures::stream::Stream;
#[allow(deprecated)] use tokio_core::io::Io;
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_core::reactor::Handle;
use bytes::{BufMut, Buf};

#[cfg(windows)]
use tokio_named_pipes::NamedPipe;

#[cfg(windows)]
mod win_permissions;

#[cfg(windows)]
pub use win_permissions::SecurityAttributes;

#[cfg(unix)]
mod unix_permissions;
#[cfg(unix)]
pub use unix_permissions::SecurityAttributes;

/// For testing/examples
pub fn dummy_endpoint() -> String {
    extern crate rand;

    let num: u64 = rand::Rng::gen(&mut rand::thread_rng());
    if cfg!(windows) {
        format!(r"\\.\pipe\my-pipe-{}", num)
    } else {
        format!(r"/tmp/my-uds-{}", num)
    }
}

/// Endpoint for IPC transport
///
/// # Examples
///
/// ``` 
/// extern crate tokio_core;
/// extern crate futures;
/// extern crate parity_tokio_ipc;
///
/// use parity_tokio_ipc::{Endpoint, dummy_endpoint};
/// use tokio_core::reactor::Core;
/// use futures::{future, Stream};
///
/// fn main() {
///     let core = Core::new().unwrap();
///     let endpoint = Endpoint::new(dummy_endpoint());
///     endpoint.incoming(core.handle())
///         .expect("failed to open a pipe")
///         .for_each(|(stream, _)| {
///             println!("Connection received");
///             future::ok(())
///     });
/// }
/// ```
pub struct Endpoint {
    path: String,
    security_attributes: SecurityAttributes,
}

impl Endpoint {
    /// Stream of incoming connections
    #[cfg(not(windows))]
    pub fn incoming(self, handle: Handle) -> io::Result<Incoming> {
        Ok(
            Incoming { inner: self.inner(&handle)?.incoming() }
          )
    }

    /// Stream of incoming connections    
    #[cfg(windows)]
    pub fn incoming(mut self, handle: Handle) -> io::Result<Incoming> {
        let pipe = self.inner(&handle)?;
        Ok(
            Incoming { inner: NamedPipeSupport { path: self.path, handle: handle.remote().clone(), pipe: pipe, security_attributes: self.security_attributes} }
          )
    }

    /// Inner platform-dependant state of the endpoint
    #[cfg(windows)]
    fn inner(&mut self, handle: &Handle) -> io::Result<NamedPipe> {
        extern crate mio_named_pipes;
        use std::os::windows::io::*;
        use miow::pipe::NamedPipeBuilder;

        let raw_handle = unsafe { NamedPipeBuilder::new(&self.path)
            .first(true)
            .inbound(true)
            .outbound(true)
            .out_buffer_size(65536)
            .in_buffer_size(65536)
            .with_security_attributes(self.security_attributes.as_ptr())?
            .into_raw_handle()};

        let mio_pipe = unsafe { mio_named_pipes::NamedPipe::from_raw_handle(raw_handle) };
        NamedPipe::from_pipe(mio_pipe, handle)
    }

    /// Inner platform-dependant state of the endpoint
    #[cfg(not(windows))]
    fn inner(&self, handle: &Handle) -> io::Result<tokio_uds::UnixListener> {
        tokio_uds::UnixListener::bind(&self.path, handle)
    }

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
            path: path,
            security_attributes: SecurityAttributes::empty(),
        }
    }
}

/// Remote connection data, if any available
pub struct RemoteId;

#[cfg(windows)]
struct NamedPipeSupport {
    path: String,
    handle: tokio_core::reactor::Remote,
    pipe: NamedPipe,
    security_attributes: SecurityAttributes,
}

#[cfg(windows)]
impl NamedPipeSupport {
    fn replacement_pipe(&mut self) -> io::Result<NamedPipe> {
        extern crate mio_named_pipes;

        use std::os::windows::io::*;
        use miow::pipe::NamedPipeBuilder;

        let ev_handle = &self.handle.handle().ok_or(
            io::Error::new(io::ErrorKind::Other, "Cannot spawn event loop handle")
        )?;


        let raw_handle = unsafe { NamedPipeBuilder::new(&self.path)
            .first(false)
            .inbound(true)
            .outbound(true)
            .out_buffer_size(65536)
            .in_buffer_size(65536)
            .with_security_attributes(self.security_attributes.as_ptr())?
            .into_raw_handle()};

        let mio_pipe = unsafe { mio_named_pipes::NamedPipe::from_raw_handle(raw_handle) };
        NamedPipe::from_pipe(mio_pipe, ev_handle)
    }
}

/// Stream of incoming connections
pub struct Incoming {
    #[cfg(not(windows))]
    #[allow(deprecated)]
    inner: ::tokio_core::io::IoStream<(tokio_uds::UnixStream, std::os::unix::net::SocketAddr)>,
    #[cfg(windows)]
    inner: NamedPipeSupport,
}

impl Stream for Incoming {
    type Item = (IpcConnection, RemoteId);
    type Error = io::Error;

    #[cfg(not(windows))]
    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        self.inner.poll().map(|poll| match poll {
            Async::Ready(Some(val)) => Async::Ready(Some((IpcConnection { inner: val.0 }, RemoteId))),
            Async::Ready(None) => Async::Ready(None),
            Async::NotReady => Async::NotReady,
        })
    }

    #[cfg(windows)]
    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        match self.inner.pipe.connect() {
            Ok(()) => {
                trace!("Incoming connection polled successfully");
                let new_listener = self.inner.replacement_pipe()?;
                Ok(Async::Ready(Some((
                        IpcConnection { 
                            inner: ::std::mem::replace(
                                &mut self.inner.pipe, 
                                new_listener,
                            ) 
                        }, 
                        RemoteId,
                ))))
            },
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    trace!("Incoming connection was to block, waiting for connection to become writeable");
                    self.inner.pipe.poll_write();
                    Ok(Async::NotReady)
                } else {
                    Err(e)
                }
            },
        }
    }      
}

/// IPC Connection
pub struct IpcConnection {
    #[cfg(windows)]
    inner: tokio_named_pipes::NamedPipe,
    #[cfg(not(windows))]
    inner: tokio_uds::UnixStream,
}

#[deprecated(since="0.1.5", note = "Please use `IpcConnection` instead")]
pub type IpcStream = IpcConnection;


impl IpcConnection {
    pub fn connect<P: AsRef<Path>>(path: P, handle: &Handle) -> io::Result<IpcConnection> {
        Ok(IpcConnection{
            inner: Self::connect_inner(path.as_ref(), handle)?,
        })
    }

    #[cfg(unix)]
    fn connect_inner(path: &Path, handle: &Handle) -> io::Result<tokio_uds::UnixStream> {
        tokio_uds::UnixStream::connect(&path, &handle)
    }

    #[cfg(windows)]
    fn connect_inner(path: &Path, handle: &Handle) -> io::Result<NamedPipe> {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::{FromRawHandle, IntoRawHandle};
        use winapi::um::winbase::FILE_FLAG_OVERLAPPED;
        miow::pipe::NamedPipe::wait(path, None)?;
        let mut options = OpenOptions::new();
        options.read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED);
        let file = options.open(path)?;
        let mio_pipe = unsafe {  mio_named_pipes::NamedPipe::from_raw_handle(file.into_raw_handle())  };
        let pipe = NamedPipe::from_pipe(mio_pipe, &handle)?;
        Ok(pipe)
    }
}

impl Read for IpcConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    } 
}

impl Write for IpcConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[allow(deprecated)]
impl Io for IpcConnection {
    fn poll_read(&mut self) -> Async<()> {
        self.inner.poll_read()
    }

    fn poll_write(&mut self) -> Async<()> {
        self.inner.poll_write()
    }
}

impl AsyncRead for IpcConnection {
    unsafe fn prepare_uninitialized_buffer(&self, b: &mut [u8]) -> bool {
        self.inner.prepare_uninitialized_buffer(b)
    }

    fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        self.inner.read_buf(buf)
    }
}

impl AsyncWrite for IpcConnection {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        AsyncWrite::shutdown(&mut self.inner)
    }

    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        self.inner.write_buf(buf)
    }
}

#[cfg(test)]
mod tests {
    extern crate rand;

    use tokio_core::reactor::Core;
    use tokio_core::io::{self, Io};
    use futures::{Stream, Future};
    use futures::sync::oneshot;
    use std::thread;

    use super::Endpoint;
    use super::IpcConnection;
    #[cfg(windows)]
    use super::SecurityAttributes;

    #[cfg(not(windows))]
    fn random_pipe_path() -> String {
        let num: u64 = self::rand::Rng::gen(&mut rand::thread_rng());
        format!(r"/tmp/parity-tokio-ipc-test-pipe-{}", num)
    }

    #[cfg(windows)]
    fn random_pipe_path() -> String {
        let num: u64 = self::rand::Rng::gen(&mut rand::thread_rng());
        format!(r"\\.\pipe\my-pipe-{}", num)
    }

    fn run_server(path: &str) {
        let path = path.to_owned();
        let (ok_signal, ok_rx) = oneshot::channel();
        thread::spawn(|| {
            let mut core = Core::new().expect("failed to spawn an event loop");
            let endpoint = Endpoint::new(path);
            let connections = endpoint.incoming(core.handle()).expect("failed to open up a new pipe/socket");
            ok_signal.send(()).expect("failed to send ok");
            let srv = connections.for_each(|(stream, _)| {
                    let (reader, writer) = stream.split();
                    let buf = [0u8; 5];
                    io::read_exact(reader,buf).and_then(move |(_reader, buf)| {
                        let mut reply = vec![];
                        reply.extend(&buf[..]);
                        io::write_all(writer, reply)
                    })
                    .map_err(|e| {trace!("io error: {:?}", e); e })
                    .map(|_| ())
                })
                .map_err(|_| ());
            core.run(srv).expect("server failed");
        });
        ok_rx.wait().expect("failed to receive handle")
    }

    #[test]
    fn smoke_test() {
        let path = random_pipe_path();
        run_server(&path);
        let mut core = Core::new().expect("failed to spawn an event loop");
        let handle = core.handle();

        let client = IpcConnection::connect(&path, &handle).expect("failed to open a client");
        let other_client = IpcConnection::connect(&path, &handle).expect("failed to open a client again");
        let msg = b"hello";

        let mut rx_buf = vec![0u8; msg.len()];
        let client_fut = io::write_all(client, &msg).and_then(|(client, _)| {
            io::read_exact(client, &mut rx_buf).map(|(_, buf)| buf)
        });

        let mut rx_buf2 = vec![0u8; msg.len()];
        let other_client_fut = io::write_all(other_client, &msg).and_then(|(client, _)| {
            io::read_exact(client, &mut rx_buf2).map(|(_, buf)| buf)
        });

        let (rx_msg, other_rx_msg) = core.run(client_fut.join(other_client_fut)).expect("failed to read from server");
        assert_eq!(rx_msg,  msg);
        assert_eq!(other_rx_msg,  msg);
    }

    #[cfg(windows)]
    fn create_pipe_with_permissions(attr: SecurityAttributes) -> ::std::io::Result<()> {
        let mut core = Core::new().expect("Failed to spawn an event loop");
        let path = random_pipe_path();

        let mut endpoint = Endpoint::new(path);
        endpoint.set_security_attributes(attr);
        endpoint.incoming(core.handle()).map(|_| ())
    }

    #[cfg(windows)]
    #[test]
    fn test_pipe_permissions() {
        create_pipe_with_permissions(SecurityAttributes::empty()).expect("failed with no attributes");
        create_pipe_with_permissions(SecurityAttributes::allow_everyone_create().unwrap())
            .expect("failed with attributes for creating");
        // create_pipe_with_permissions(SecurityAttributes::allow_everyone_create().unwrap())
        //     .expect("failed with attributes for creating");
        // create_pipe_with_permissions(SecurityAttributes::allow_everyone_connect().unwrap())
        //     .expect("failed with attributes for connecting");
        // create_pipe_with_permissions(SecurityAttributes::allow_everyone_connect().unwrap())
        //     .expect("failed with attributes for connecting");
    }
}
