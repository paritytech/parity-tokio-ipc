//! Tokio IPC transport. Under the hood uses Unix Domain Sockets for Linux/Mac 
//! and Named Pipes for Windows. 

extern crate futures;
extern crate tokio_uds;
extern crate tokio_named_pipes;
extern crate tokio_core;
extern crate tokio_io;
extern crate bytes;
#[macro_use] extern crate log;

#[cfg(windows)] 
extern crate miow;

use std::io::{self, Read, Write};

use futures::{Async, Poll};
use futures::stream::Stream;
#[allow(deprecated)] use tokio_core::io::Io;
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_core::reactor::Handle;
use bytes::{BufMut, Buf};

#[cfg(windows)]
use tokio_named_pipes::NamedPipe;

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
///     let endpoint = Endpoint::new(dummy_endpoint(), &core.handle()).unwrap();
///     endpoint.incoming().for_each(|(stream, _)| {
///         println!("Connection received");
///         future::ok(())
///     });
/// }
/// ```
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

#[cfg(windows)]
fn replacement_pipe(path: &str, handle: &Handle) -> io::Result<NamedPipe> {
    extern crate mio_named_pipes;

    use std::os::windows::io::*;
    use miow::pipe::NamedPipeBuilder;

    let raw_handle = NamedPipeBuilder::new(path)
        .first(false)
        .inbound(true)
        .outbound(true)
        .out_buffer_size(65536)
        .in_buffer_size(65536)
        .create()?
        .into_raw_handle();

    let mio_pipe = unsafe { mio_named_pipes::NamedPipe::from_raw_handle(raw_handle) };
    
    NamedPipe::from_pipe(mio_pipe, handle)
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
            Ok(()) => {
                trace!("Incoming connection polled successfully");
                let handle = &self.inner.handle.handle().ok_or(
                    io::Error::new(io::ErrorKind::Other, "Cannot spawn event loop handle")
                )?;
                Ok(Async::Ready(Some((
                    (
                        IpcStream { 
                            inner: ::std::mem::replace(
                                &mut self.inner.pipe, 
                                replacement_pipe(&self.inner.path, &handle)?,
                            ) 
                        }, 
                        RemoteId,
                    )
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

#[allow(deprecated)]
impl Io for IpcStream {
    fn poll_read(&mut self) -> Async<()> {
        self.inner.poll_read()
    }

    fn poll_write(&mut self) -> Async<()> {
        self.inner.poll_write()
    }
}

impl AsyncRead for IpcStream {
    unsafe fn prepare_uninitialized_buffer(&self, b: &mut [u8]) -> bool {
        self.inner.prepare_uninitialized_buffer(b)
    }

    fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        self.inner.read_buf(buf)
    }
}

impl AsyncWrite for IpcStream {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        self.inner.shutdown()
    }

    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        self.inner.write_buf(buf)
    }
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    extern crate rand;

    use std::thread;
    use tokio_core::reactor::Core;
    use tokio_core::io::{self, Io};
    use futures::{Stream, Future};

    use super::Endpoint;

    #[cfg(windows)]
    fn random_pipe_path() -> String {
        let num: u64 = self::rand::Rng::gen(&mut rand::thread_rng());
        format!(r"\\.\pipe\my-pipe-{}", num)
    }

    pub fn dummy_request(addr: &str, buf: &[u8]) -> Vec<u8> {
        use miow;
        use std::io::{Read, Write};
        use std::fs::OpenOptions;

        miow::pipe::NamedPipe::wait(addr, None).unwrap();
        let mut f = OpenOptions::new().read(true).write(true).open(addr).unwrap();
        trace!("Connected");
        f.write_all(buf).unwrap();
        f.flush().unwrap();
        trace!("Wrote");

        let mut buf = vec![0u8; 65536];
        let sz = f.read(&mut buf).unwrap_or_else(|_| { 0 });
        (&buf[0..sz]).to_vec()
    }

    #[test]
    #[cfg(windows)]
    fn win_smoky() {
        let path = random_pipe_path(); let path2 = path.clone();

        thread::spawn(move || {
            let mut core = Core::new().expect("Server event loop should start ok");
            let endpoint = Endpoint::new(path, &core.handle()).expect("Should be created");
            let srv = endpoint.incoming()
                .for_each(|(stream, _)| {
                    trace!("Created connection");                   
                    let (reader, writer) = stream.split();
                    let buf = [0u8; 6];
                    io::read_exact(reader, buf).and_then(move |(_reader, _buf)| { 
                            let mut reply = Vec::new(); 
                            reply.extend(&_buf[..]);
                            reply.extend(b" - Ok");
                            io::write_all(writer, reply)
                        })
                        .map_err(|e| { trace!("io error: {:?}", e); e })
                        .map(|_| ())
                })
                .map(|_| ())
                .map_err(|e|{ trace!("io error: {:?}", e); () })
                .boxed();

            core.run(srv).expect("Server event loop should finish ok");
        });
        thread::sleep(::std::time::Duration::from_millis(50));

        let res = dummy_request(&path2, b"Space1");
        assert_eq!(res, b"Space1 - Ok");

        let res = dummy_request(&path2, b"Space2");
        assert_eq!(res, b"Space2 - Ok");        
    }


}