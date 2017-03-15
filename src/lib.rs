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

#[cfg(test)]
#[cfg(windows)]
mod tests {
    extern crate rand;
    extern crate tokio_line;

    use std::thread;
    use tokio_core::reactor::{Core, Handle};
    use tokio_core::io::{self, Io};
    use futures::{future, Stream, Sink, Future};

    use super::Endpoint;

    #[cfg(windows)]
    fn random_pipe_path() -> String {
        let num: u64 = self::rand::Rng::gen(&mut rand::thread_rng());
        format!(r"\\.\pipe\my-pipe-{}", num)
    }

    pub fn dummy_request(addr: &str, buf: &[u8]) -> Vec<u8> {
        extern crate miow;

        use std::io::{Read, Write};
        use std::fs::OpenOptions;

        miow::pipe::NamedPipe::wait(addr, None).unwrap();
        let mut f = OpenOptions::new().read(true).write(true).open(addr).unwrap();
        println!("Connected");
        f.write_all(buf).unwrap();
        f.flush().unwrap();
        println!("Wrote");

        let mut buf = vec![0u8; 65536];
        let sz = f.read(&mut buf).unwrap_or_else(|_| { 0 });
        (&buf[0..sz]).to_vec()
    }

    #[test]
    #[cfg(windows)]
    fn win_smoky() {
        let path = random_pipe_path(); let path2 = path.clone();

        thread::spawn(move || {
            let mut core = Core::new().unwrap();
            let endpoint = Endpoint::new(path, &core.handle()).expect("Should be created");
            let srv = endpoint.incoming()
                .for_each(|(stream, _)| {
                    println!("Created connection");                   
                    let (reader, writer) = stream.split();
                    let mut buf = Vec::new();
                    io::read_to_end(reader, buf).and_then(move |(reader, buf)| io::write_all(writer, "Ok"))
                        .map(|_| ())
                })
                .map(|_| ())
                .map_err(|_e| ())
                .boxed();

            core.run(srv).unwrap();
        });
        thread::sleep(::std::time::Duration::from_millis(50));

        dummy_request(&path2, b"Space\n\r");
    }


}