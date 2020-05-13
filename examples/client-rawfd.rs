#[tokio::main]
async fn main() {
	#[cfg(unix)]
	{
		let path = "/tmp/2";
		let path = path.to_owned() + &"\x00".to_string();
		use nix::sys::socket::*;
		use tokio::{self, prelude::*};

		let fd = socket(
			AddressFamily::Unix,
			SockType::Stream,
			SockFlag::empty(),
			None,
		)
		.unwrap();

		let sockaddr = UnixAddr::new_abstract(path.as_bytes()).unwrap();
		let sockaddr = SockAddr::Unix(sockaddr);
		connect(fd, &sockaddr).unwrap();

		let mut client = parity_tokio_ipc::Connection::from_raw_fd(fd);

		loop {
			let mut buf = [0u8; 4];
			println!("SEND: PING");
			client
				.write_all(b"ping")
				.await
				.expect("Unable to write message to client");
			client
				.read_exact(&mut buf[..])
				.await
				.expect("Unable to read buffer");
			if let Ok("pong") = std::str::from_utf8(&buf[..]) {
				println!("RECEIVED: PONG");
			} else {
				break;
			}

			tokio::time::delay_for(std::time::Duration::from_secs(2)).await;
		}
	}
}
