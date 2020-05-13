#[cfg(unix)]
pub fn bind(host: &String) -> std::os::unix::io::RawFd {
	use nix::fcntl::{fcntl, FcntlArg, OFlag};
	use nix::sys::socket::{bind as nix_bind, *};

	let hostv: Vec<&str> = host.trim().split("://").collect();
	if hostv.len() != 2 {
		panic!("Host {} is not right", host);
	}
	let scheme = hostv[0].to_lowercase();

	let sockaddr: SockAddr;
	let fd: std::os::unix::io::RawFd;

	match scheme.as_str() {
		"unix" => {
			fd = socket(
				AddressFamily::Unix,
				SockType::Stream,
				SockFlag::SOCK_CLOEXEC,
				None,
			)
			.unwrap();
			let sockaddr_h = hostv[1].to_owned() + &"\x00".to_string();
			let sockaddr_u = UnixAddr::new_abstract(sockaddr_h.as_bytes()).unwrap();
			sockaddr = SockAddr::Unix(sockaddr_u);
		}

		"vsock" => {
			let host_port_v: Vec<&str> = hostv[1].split(':').collect();
			if host_port_v.len() != 2 {
				panic!();
			}
			let cid = libc::VMADDR_CID_ANY;
			let port: u32 = std::str::FromStr::from_str(host_port_v[1])
				.expect("the vsock port is not an number");
			fd = socket(
				AddressFamily::Vsock,
				SockType::Stream,
				SockFlag::SOCK_CLOEXEC,
				None,
			)
			.unwrap();
			sockaddr = SockAddr::new_vsock(cid, port);
		}
		_ => panic!("Scheme {} is not supported", scheme),
	};

	nix_bind(fd, &sockaddr).unwrap();
	listen(fd, 10).unwrap();

	fcntl(fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();

	fd
}

#[cfg(unix)]
async fn run_server(path: String) {
	use futures::StreamExt as _;
	use parity_tokio_ipc::Endpoint;
	use tokio::{io::split, prelude::*};

	let mut endpoint = Endpoint::from_raw_fd(bind(&path));
	let mut incoming = endpoint.incoming().expect("failed to open new socket");

	while let Some(result) = incoming.next().await {
		match result {
			Ok(stream) => {
				let (mut reader, mut writer) = split(stream);

				tokio::spawn(async move {
					loop {
						let mut buf = [0u8; 4];
						let pong_buf = b"pong";
						if let Err(_) = reader.read_exact(&mut buf).await {
							println!("Closing socket");
							break;
						}
						if let Ok("ping") = std::str::from_utf8(&buf[..]) {
							println!("RECIEVED: PING");
							writer
								.write_all(pong_buf)
								.await
								.expect("unable to write to socket");
							println!("SEND: PONG");
						}
					}
				});
			}
			Err(e) => println!("{:?}", e),
		}
	}
}

#[tokio::main]
async fn main() {
	#[cfg(unix)]
	{
		let path = "unix:///tmp/2";
		run_server(path.to_string()).await
	}
}
