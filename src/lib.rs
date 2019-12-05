//! Tokio IPC transport. Under the hood uses Unix Domain Sockets for Linux/Mac
//! and Named Pipes for Windows.

#![warn(missing_docs)]
//#![deny(rust_2018_idioms)]

#[cfg(windows)]
mod win;
#[cfg(not(windows))]
mod unix;

/// For testing/examples
pub fn dummy_endpoint() -> String {
	let num: u64 = rand::Rng::gen(&mut rand::thread_rng());
	if cfg!(windows) {
		format!(r"\\.\pipe\my-pipe-{}", num)
	} else {
		format!(r"~/Desktop/jsonrpc.ipc")
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
///     let mut endpoint = Endpoint::new(dummy_endpoint());
///     let server = endpoint.incoming()
///         .expect("failed to open up a new pipe/socket")
///         .for_each(|_stream| {
///             println!("Connection received");
///             ()
///         });
///     // ... run server etc.
/// }
///
#[cfg(windows)]
pub use win::{SecurityAttributes, Endpoint};
#[cfg(unix)]
pub use unix::{SecurityAttributes, Endpoint};

#[cfg(test)]
mod tests {
	use tokio::prelude::*;
	use futures::{channel::oneshot, Future, Stream, StreamExt as _, FutureExt as _};
	use std::thread;
	use std::time::Duration;
	use tokio::{
        self,
        io::{self, AsyncRead, split},
        net::UnixStream,
    };

	use super::{dummy_endpoint, Endpoint, SecurityAttributes};
	use futures::channel::oneshot::Sender;
	use std::path::Path;
	use tokio::runtime::Runtime;
	use futures::future::Either;

	fn run_server(path: &str, runtime: &mut Runtime) -> Sender<()> {
		let path = path.to_owned();
		let (shutdown_tx, shutdown_rx) = oneshot::channel();
		let mut endpoint = Endpoint::new(path);

		endpoint.set_security_attributes(
			SecurityAttributes::empty()
				.set_mode(0o777)
				.unwrap()
		);

        let server = async move {
	        println!("\n\n\n\nPolling\n\n\n");
            while let Some(result) = endpoint.incoming().expect("failed to open up a new socket").next().await {
                match result {
                    Ok(stream) => {
                        let (mut reader, mut writer) = split(stream);
                        let mut buf = [0u8; 5];
                        reader.read_exact(&mut buf).await;
                        writer.write_all(&buf[..]).await;
                    }
                    _ => unreachable!("also ideally")
                }
            };
        };

		runtime.spawn(
			futures::future::select(Box::pin(server), shutdown_rx)
				.then(|either| {
					match either {
						Either::Right((_, server)) => {
							drop(server);
						}
						_ => unreachable!("ideally")
					};
					futures::future::ready(())
				})
		);
		println!("Server running.");
		shutdown_tx
	}

	// NOTE: Intermittently fails or stalls on windows.
	#[tokio::test]
	async fn smoke_test() {
		let mut runtime = Runtime::new().expect("Error creating tokio runtime");
		let path = dummy_endpoint();
		let mut shutdown = run_server(&path, &mut runtime);

		tokio::time::delay_for(Duration::from_secs(5)).await;

		println!("Connecting to client 0...");
		let mut client_0 = UnixStream::connect(&path).await
			.expect("failed to open client_0");
		println!("Connecting to client 1...");
		let mut client_1 = UnixStream::connect(&path).await
			.expect("failed to open client_1");
		let msg = b"hello";

		let mut rx_buf = vec![0u8; msg.len()];
		client_0.write_all(msg).await;
		client_0.read_exact(&mut rx_buf).await;

		let mut rx_buf2 = vec![0u8; msg.len()];
		client_1.write_all(msg).await;
		client_1.read_exact(&mut rx_buf2).await;


		assert_eq!(rx_buf, msg);
		assert_eq!(rx_buf2, msg);

		// shutdown server
		if let Ok(()) = shutdown.send(()) {
			// wait one second for the file to be deleted.
			thread::sleep(Duration::from_secs(1));
			let path = Path::new(&path);
			// assert that it has
			assert!(!path.exists());
		}
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
		create_pipe_with_permissions(SecurityAttributes::empty().allow_everyone_connect().unwrap())
			.expect("failed with attributes for connecting");
	}
}
