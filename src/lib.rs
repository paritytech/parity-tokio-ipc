//! Tokio IPC transport. Under the hood uses Unix Domain Sockets for Linux/Mac
//! and Named Pipes for Windows.

#![warn(missing_docs)]
//#![deny(rust_2018_idioms)]

// Use this directly once Rust 1.54 is stabilized; for some reason going
// indirectly through a macro is okay.
// See https://github.com/rust-lang/rust/issues/78835
macro_rules! doc_comment {
    ($x:expr) => {
        #[doc = $x]
        extern "C" {}
    };
}

doc_comment!(include_str!("../README.md"));

#[cfg(not(windows))]
mod unix;
#[cfg(windows)]
mod win;

#[cfg(unix)]
pub use unix::{Connection, Endpoint, SecurityAttributes};
/// Endpoint for IPC transport
///
/// # Examples
///
/// ```ignore
/// use parity_tokio_ipc::{Endpoint, dummy_endpoint};
/// use futures::{future, Future, Stream, StreamExt};
/// use tokio::runtime::Runtime;
///
/// fn main() {
///		let mut runtime = Runtime::new().unwrap();
///     let mut endpoint = Endpoint::new(dummy_endpoint());
///     let server = endpoint.incoming()
///         .expect("failed to open up a new pipe/socket")
///         .for_each(|_stream| {
///             println!("Connection received");
///             futures::future::ready(())
///         });
///		runtime.block_on(server)
/// }
///```
#[cfg(windows)]
pub use win::{Connection, Endpoint, SecurityAttributes};

/// For testing/examples
#[must_use]
pub fn dummy_endpoint() -> String {
    let num: u64 = rand::Rng::gen(&mut rand::thread_rng());
    if cfg!(windows) {
        format!(r"\\.\pipe\my-pipe-{}", num)
    } else {
        format!(r"/tmp/my-uds-{}", num)
    }
}

#[cfg(test)]
mod tests {
    use futures::{channel::oneshot, FutureExt as _, StreamExt as _};
    use std::time::Duration;
    use tokio::io::{split, AsyncReadExt, AsyncWriteExt};

    use super::{dummy_endpoint, Endpoint, SecurityAttributes};
    use futures::future::{ready, select, Either};
    use std::path::Path;

    async fn run_server(path: String) {
        let path = path.clone();
        let mut endpoint = Endpoint::new(path);

        endpoint.set_security_attributes(SecurityAttributes::empty().set_mode(0o777).unwrap());
        let incoming = endpoint.incoming().expect("failed to open up a new socket");
        futures::pin_mut!(incoming);

        while let Some(result) = incoming.next().await {
            match result {
                Ok(stream) => {
                    let (mut reader, mut writer) = split(stream);
                    let mut buf = [0_u8; 5];
                    reader
                        .read_exact(&mut buf)
                        .await
                        .expect("unable to read from socket");
                    writer
                        .write_all(&buf[..])
                        .await
                        .expect("unable to write to socket");
                }
                _ => unreachable!("ideally"),
            }
        }
    }

    #[tokio::test]
    async fn smoke_test() {
        let path = dummy_endpoint();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let server = select(Box::pin(run_server(path.clone())), shutdown_rx).then(|either| {
            match either {
                Either::Right((_, server)) => {
                    drop(server);
                }
                _ => unreachable!("also ideally"),
            };
            ready(())
        });
        tokio::spawn(server);

        tokio::time::sleep(Duration::from_secs(2)).await;

        println!("Connecting to client 0...");
        let mut client_0 = Endpoint::connect(&path)
            .await
            .expect("failed to open client_0");
        tokio::time::sleep(Duration::from_secs(2)).await;
        println!("Connecting to client 1...");
        let mut client_1 = Endpoint::connect(&path)
            .await
            .expect("failed to open client_1");
        let msg = b"hello";

        let mut rx_buf = vec![0_u8; msg.len()];
        client_0
            .write_all(msg)
            .await
            .expect("Unable to write message to client");
        client_0
            .read_exact(&mut rx_buf)
            .await
            .expect("Unable to read message from client");

        let mut rx_buf2 = vec![0_u8; msg.len()];
        client_1
            .write_all(msg)
            .await
            .expect("Unable to write message to client");
        client_1
            .read_exact(&mut rx_buf2)
            .await
            .expect("Unable to read message from client");

        assert_eq!(rx_buf, msg);
        assert_eq!(rx_buf2, msg);

        // shutdown server
        if let Ok(()) = shutdown_tx.send(()) {
            // wait one second for the file to be deleted.
            tokio::time::sleep(Duration::from_secs(1)).await;
            let path = Path::new(&path);
            // assert that it has
            assert!(!path.exists());
        }
    }

    #[tokio::test]
    async fn incoming_stream_is_static() {
        fn is_static<T: 'static>(_: T) {}

        let path = dummy_endpoint();
        let endpoint = Endpoint::new(path);
        is_static(endpoint.incoming());
    }

    #[cfg(windows)]
    fn create_pipe_with_permissions(attr: SecurityAttributes) -> ::std::io::Result<()> {
        let path = dummy_endpoint();

        let mut endpoint = Endpoint::new(path);
        endpoint.set_security_attributes(attr);
        endpoint.incoming().map(|_| ())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_pipe_permissions() {
        create_pipe_with_permissions(SecurityAttributes::empty())
            .expect("failed with no attributes");
        create_pipe_with_permissions(SecurityAttributes::allow_everyone_create().unwrap())
            .expect("failed with attributes for creating");
        create_pipe_with_permissions(SecurityAttributes::allow_everyone_connect().unwrap())
            .expect("failed with attributes for connecting");
    }
}
