//! Tokio IPC transport. Under the hood uses Unix Domain Sockets for Linux/Mac 
//! and Named Pipes for Windows. 

extern crate futures;
extern crate tokio_uds;
extern crate tokio_named_pipes;
#[macro_use] extern crate tokio_core;

use std::io::{self, Read, Write};

use futures::{Async, Poll};
use futures::stream::Stream;
use tokio_core::io::Io;
use tokio_core::reactor::Handle;

#[cfg(windows)]
use tokio_named_pipes::NamedPipe;

/// IPC Endpoint (UnixListener or rolling NamedPipe)
pub struct Endpoint {
    _path: String,
    _handle: Handle,
    #[cfg(not(windows))]
    inner: tokio_uds::UnixListener,
    #[cfg(windows)]
    inner: NamedPipe,
}

impl Endpoint {
    /// Stream of incoming connections
    #[cfg(not(windows))]
    pub fn incoming(self) -> Incoming {
        Incoming { inner: self.inner.incoming() }
    }

    /// Stream of incoming connections    
    #[cfg(windows)]
    pub fn incoming(self) -> Incoming {
        Incoming { inner: NamedPipeSupport { path: self._path, handle: self._handle.remote().clone(), pipe: self.inner } }
    }

    /// Inner platform-dependant state of the endpoint
    #[cfg(windows)]
    fn inner(p: &str, handle: &Handle) -> io::Result<NamedPipe> {
        NamedPipe::new(p, handle)
    }

    /// Inner platform-dependant state of the endpoint
    #[cfg(not(windows))]
    fn inner(p: &str, handle: &Handle) -> io::Result<tokio_uds::UnixListener> {
        tokio_uds::UnixListener::bind(p, handle)
    }

    /// New IPC endpoint at the given path
    /// Endpoint ready to accept connections immediately
    pub fn new(path: String, handle: &Handle) -> io::Result<Self> {
        Ok(Endpoint { 
            inner: Self::inner(&path, handle)?,
            _path: path, 
            _handle: handle.clone(),
        })
    }
}

/// Remote connection data, if any available
pub struct RemoteId;

#[cfg(windows)]
struct NamedPipeSupport {
    path: String,
    handle: tokio_core::reactor::Remote,
    pipe: NamedPipe,    
}

/// Stream of incoming connections
pub struct Incoming {
    #[cfg(not(windows))]
    inner: ::tokio_core::io::IoStream<(tokio_uds::UnixStream, std::os::unix::net::SocketAddr)>,
    #[cfg(windows)]
    inner: NamedPipeSupport,
}

impl Stream for Incoming {
    type Item = (IpcStream, RemoteId);
    type Error = io::Error;

    #[cfg(not(windows))]
    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        self.inner.poll().map(|poll| match poll {
            Async::Ready(Some(val)) => Async::Ready(Some((IpcStream { inner: val.0 }, RemoteId))),
            Async::Ready(None) => Async::Ready(None),
            Async::NotReady => Async::NotReady,
        })
    }    

    #[cfg(windows)]
    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        match self.inner.pipe.connect() {
            Ok(()) => Ok(Async::Ready(Some((
                (
                    IpcStream { 
                        inner: ::std::mem::replace(
                            &mut self.inner.pipe, 
                            NamedPipe::new(&self.inner.path, 
                                &self.inner.handle.handle().ok_or(
                                    io::Error::new(io::ErrorKind::Other, "Cannot spawn event loop handle")
                                )?
                            )?,
                        ) 
                    }, 
                    RemoteId,
                )
            )))),
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    ::futures::task::park();
                    Ok(Async::NotReady)
                } else {
                    Err(e)
                }
            },
        }
    }      
}

/// IPC stream of client connection
pub struct IpcStream {
    #[cfg(windows)]
    inner: tokio_named_pipes::NamedPipe,
    #[cfg(not(windows))]
    inner: tokio_uds::UnixStream,
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    } 
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Io for IpcStream {
    fn poll_read(&mut self) -> Async<()> {
        self.inner.poll_read()
    }

    fn poll_write(&mut self) -> Async<()> {
        self.inner.poll_write()
    }
}