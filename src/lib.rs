//! Tokio IPC transport. Under the hood uses Unix Domain Sockets for Linux/Mac
//! and Named Pipes for Windows.

#![warn(missing_docs)]
#![deny(rust_2018_idioms)]

use std::io::{self, Read, Write};
use std::path::Path;

use bytes::{Buf, BufMut};
use futures::{stream::Stream, Async, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::reactor::Handle;

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

#[cfg(windows)]
const PIPE_AVAILABILITY_TIMEOUT: u64 = 5000;

/// For testing/examples
pub fn dummy_endpoint() -> String {
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
/// extern crate tokio;
/// extern crate futures;
/// extern crate parity_tokio_ipc;
///
/// use parity_tokio_ipc::{Endpoint, dummy_endpoint};
/// use futures::{future, Future, Stream};
///
/// fn main() {
///     let runtime = tokio::runtime::Runtime::new()
///         .expect("Error creating tokio runtime");
///     let handle = runtime.reactor();
///     let endpoint = Endpoint::new(dummy_endpoint());
///     let server = endpoint.incoming(handle)
///         .expect("failed to open up a new pipe/socket")
///         .for_each(|(_stream, _remote_id)| {
///             println!("Connection received");
///             future::ok(())
///         })
///         .map_err(|err| panic!("Endpoint connection error: {:?}", err));
///     // ... run server etc.
/// }
/// ```
pub struct Endpoint {
    path: String,
    security_attributes: SecurityAttributes,
}

impl Endpoint {
    /// Stream of incoming connections
    #[cfg(not(windows))]
    pub fn incoming(self, handle: &Handle) -> io::Result<Incoming> {
        Ok(Incoming {
            inner: self.inner(handle)?.incoming(),
        })
    }

    /// Stream of incoming connections
    #[cfg(windows)]
    pub fn incoming(mut self, handle: &Handle) -> io::Result<Incoming> {
        let pipe = self.inner(handle)?;
        Ok(Incoming {
            inner: NamedPipeSupport {
                path: self.path,
                handle: handle.clone(),
                pipe,
                security_attributes: self.security_attributes,
            },
        })
    }

    /// Inner platform-dependant state of the endpoint
    #[cfg(windows)]
    fn inner(&mut self, handle: &Handle) -> io::Result<NamedPipe> {
        use miow::pipe::NamedPipeBuilder;
        use std::os::windows::io::*;

        let raw_handle = unsafe {
            NamedPipeBuilder::new(&self.path)
                .first(true)
                .inbound(true)
                .outbound(true)
                .out_buffer_size(65536)
                .in_buffer_size(65536)
                .with_security_attributes(self.security_attributes.as_ptr())?
                .into_raw_handle()
        };

        let mio_pipe = unsafe { mio_named_pipes::NamedPipe::from_raw_handle(raw_handle) };
        NamedPipe::from_pipe(mio_pipe, handle)
    }

    /// Inner platform-dependant state of the endpoint
    #[cfg(not(windows))]
    fn inner(&self, _handle: &Handle) -> io::Result<tokio_uds::UnixListener> {
        tokio_uds::UnixListener::bind(&self.path)
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
        }
    }
}

/// Remote connection data, if any available
pub struct RemoteId;

#[cfg(windows)]
struct NamedPipeSupport {
    path: String,
    handle: Handle,
    pipe: NamedPipe,
    security_attributes: SecurityAttributes,
}

#[cfg(windows)]
impl NamedPipeSupport {
    fn replacement_pipe(&mut self) -> io::Result<NamedPipe> {
        use miow::pipe::NamedPipeBuilder;
        use std::os::windows::io::*;

        let raw_handle = unsafe {
            NamedPipeBuilder::new(&self.path)
                .first(false)
                .inbound(true)
                .outbound(true)
                .out_buffer_size(65536)
                .in_buffer_size(65536)
                .with_security_attributes(self.security_attributes.as_ptr())?
                .into_raw_handle()
        };

        let mio_pipe = unsafe { mio_named_pipes::NamedPipe::from_raw_handle(raw_handle) };
        NamedPipe::from_pipe(mio_pipe, &self.handle)
    }
}

/// Stream of incoming connections
pub struct Incoming {
    #[cfg(not(windows))]
    inner: tokio_uds::Incoming,
    #[cfg(windows)]
    inner: NamedPipeSupport,
}

impl Stream for Incoming {
    type Item = (IpcConnection, RemoteId);
    type Error = io::Error;

    #[cfg(not(windows))]
    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        self.inner.poll().map(|poll| match poll {
            Async::Ready(Some(val)) => Async::Ready(Some((IpcConnection { inner: val }, RemoteId))),
            Async::Ready(None) => Async::Ready(None),
            Async::NotReady => Async::NotReady,
        })
    }

    #[cfg(windows)]
    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        match self.inner.pipe.connect() {
            Ok(()) => {
                log::trace!("Incoming connection polled successfully");
                let new_listener = self.inner.replacement_pipe()?;
                Ok(Async::Ready(Some((
                    IpcConnection {
                        inner: ::std::mem::replace(&mut self.inner.pipe, new_listener),
                    },
                    RemoteId,
                ))))
            }
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    log::trace!("Incoming connection was to block, waiting for connection to become writeable");
                    self.inner.pipe.poll_write_ready()?;
                    Ok(Async::NotReady)
                } else {
                    Err(e)
                }
            }
        }
    }
}

/// IPC Connection
pub struct IpcConnection {
    #[cfg(not(windows))]
    inner: tokio_uds::UnixStream,
    #[cfg(windows)]
    inner: tokio_named_pipes::NamedPipe,
}

impl IpcConnection {
    /// Make new connection using the provided path and running event pool.
    pub fn connect<P: AsRef<Path>>(path: P, handle: &Handle) -> io::Result<IpcConnection> {
        Ok(IpcConnection {
            inner: Self::connect_inner(path.as_ref(), handle)?,
        })
    }

    #[cfg(unix)]
    fn connect_inner(path: &Path, _handle: &Handle) -> io::Result<tokio_uds::UnixStream> {
        use futures::Future;
        tokio_uds::UnixStream::connect(&path).wait()
    }

    #[cfg(windows)]
    fn connect_inner(path: &Path, handle: &Handle) -> io::Result<NamedPipe> {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::{FromRawHandle, IntoRawHandle};
        use winapi::um::winbase::FILE_FLAG_OVERLAPPED;

        // Wait for the pipe to become available or fail after 5 seconds.
        miow::pipe::NamedPipe::wait(
            path,
            Some(std::time::Duration::from_millis(PIPE_AVAILABILITY_TIMEOUT)),
        )?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(path)?;
        let mio_pipe =
            unsafe { mio_named_pipes::NamedPipe::from_raw_handle(file.into_raw_handle()) };
        let pipe = NamedPipe::from_pipe(mio_pipe, handle)?;
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
    use futures::{sync::oneshot, Future, Stream};
    use std::thread;
    use tokio::{
        self,
        io::{self, AsyncRead},
        reactor::Handle,
        runtime::TaskExecutor,
    };

    #[cfg(windows)]
    use super::SecurityAttributes;
    use super::{dummy_endpoint, Endpoint, IpcConnection};

    fn run_server(path: &str, exec: TaskExecutor, handle: Handle) {
        let path = path.to_owned();
        let (ok_signal, ok_rx) = oneshot::channel();
        thread::spawn(move || {
            let endpoint = Endpoint::new(path);
            let connections = endpoint
                .incoming(&handle)
                .expect("failed to open up a new pipe/socket");
            let srv = connections
                .for_each(|(stream, _)| {
                    let (reader, writer) = stream.split();
                    let buf = [0u8; 5];
                    io::read_exact(reader, buf)
                        .and_then(move |(_reader, buf)| {
                            let mut reply = vec![];
                            reply.extend(&buf[..]);
                            io::write_all(writer, reply)
                        })
                        .map_err(|e| {
                            log::trace!("io error: {:?}", e);
                            e
                        })
                        .map(|_| ())
                })
                .map_err(|_| ());
            exec.spawn(srv);
            ok_signal.send(()).expect("failed to send ok");
            println!("Server running.");
        });
        ok_rx.wait().expect("failed to receive handle")
    }

    // NOTE: Intermittently fails or stalls on windows.
    #[test]
    fn smoke_test() {
        let mut runtime = tokio::runtime::Runtime::new().expect("Error creating tokio runtime");
        let exec = runtime.executor();
        #[allow(deprecated)]
        let handle = runtime.reactor().clone();

        let path = dummy_endpoint();

        run_server(&path, exec, handle.clone());

        println!("Connecting to client 0...");
        let client_0 = IpcConnection::connect(&path, &handle).expect("failed to open client_0");
        println!("Connecting to client 1...");
        let client_1 = IpcConnection::connect(&path, &handle).expect("failed to open client_1");
        let msg = b"hello";

        let rx_buf = vec![0u8; msg.len()];
        let client_0_fut = io::write_all(client_0, msg)
            .map_err(|err| panic!("Client 0 write error: {:?}", err))
            .and_then(move |(client, _)| {
                io::read_exact(client, rx_buf)
                    .map(|(_, buf)| buf)
                    .map_err(|err| panic!("Client 0 read error: {:?}", err))
            });

        let rx_buf2 = vec![0u8; msg.len()];
        let client_1_fut = io::write_all(client_1, msg)
            .map_err(|err| panic!("Client 1 write error: {:?}", err))
            .and_then(move |(client, _)| {
                io::read_exact(client, rx_buf2)
                    .map(|(_, buf)| buf)
                    .map_err(|err| panic!("Client 1 read error: {:?}", err))
            });

        let fut = client_0_fut
            .join(client_1_fut)
            .and_then(move |(rx_msg, other_rx_msg)| {
                assert_eq!(rx_msg, msg);
                assert_eq!(other_rx_msg, msg);
                Ok(())
            })
            .map_err(|err| panic!("Smoke test error: {:?}", err));

        runtime.block_on(fut).expect("Runtime error")
    }

    #[cfg(windows)]
    fn create_pipe_with_permissions(attr: SecurityAttributes) -> ::std::io::Result<()> {
        let runtime = tokio::runtime::Runtime::new().expect("Error creating tokio runtime");
        #[allow(deprecated)]
        let handle = runtime.reactor();

        let path = dummy_endpoint();

        let mut endpoint = Endpoint::new(path);
        endpoint.set_security_attributes(attr);
        endpoint.incoming(handle).map(|_| ())
    }

    #[cfg(windows)]
    #[test]
    fn test_pipe_permissions() {
        create_pipe_with_permissions(SecurityAttributes::empty())
            .expect("failed with no attributes");
        create_pipe_with_permissions(SecurityAttributes::allow_everyone_create().unwrap())
            .expect("failed with attributes for creating");
        create_pipe_with_permissions(SecurityAttributes::allow_everyone_connect().unwrap())
            .expect("failed with attributes for connecting");
    }
}
