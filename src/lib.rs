use rustls::{OwnedTrustAnchor, ClientConfig, RootCertStore, ClientConnection};
use anyhow::{Result as AResult, anyhow};
use std::sync::Arc;
use std::net::TcpStream;
use std::io::{Read, Write};
use std::process::Command;
use std::time::Duration;
use std::path::PathBuf;
use clap::Parser;

#[derive(Parser)]
#[command(about = "Uses IMAP IDLE to run a command whenever a new email arrives", long_about = None)]
pub struct Cli {
	/// IMAP server domain
	#[arg(short, long)]
	server: String,

	/// IMAP server port
	#[arg(long, default_value_t = 993)]
	port: u16,

	/// IMAP user name
	#[arg(short, long)]
	username: String,

	/// IMAP password
	#[arg(short, long)]
	password: String,

	/// command to run when new mail arrives
	#[arg(short, long)]
	command: PathBuf,

	#[arg(short, long, action = clap::ArgAction::Count)]
	verbose: u8,
}

#[derive(PartialEq, Eq, Debug)]
enum State {
	Unauthenticated,
	Authenticated,
	Inbox,
	Idling
}

pub fn run(cli: &Cli) -> AResult<()> {
	let tls_config = ClientConfig::builder()
		.with_safe_defaults()
		.with_root_certificates(RootCertStore {
			roots: webpki_roots::TLS_SERVER_ROOTS.0.iter()
				.map(|ta| OwnedTrustAnchor::from_subject_spki_name_constraints(
					ta.subject, ta.spki, ta.name_constraints))
				.collect()
		})
		.with_no_client_auth();

	let mut buffer = [0; 2048];

	let mut tls_client = ClientConnection::new(
		Arc::new(tls_config),
		cli.server.as_str().try_into().unwrap())?;
	let mut socket = TcpStream::connect((cli.server.as_str(), cli.port))?;
	let mut state = State::Unauthenticated;

	socket.set_read_timeout(Some(Duration::from_secs(10*60)))?;
//	println!("read timeout = {:?}, write timeout = {:?}", socket.read_timeout(), socket.write_timeout());

	loop {
//		println!("wants_read = {}, wants_write = {}, is_handshaking = {}",
//				 tls_client.wants_read(),
//				 tls_client.wants_write(),
//				 tls_client.is_handshaking());

		if tls_client.is_handshaking() {
			let (_i, _o) = tls_client.complete_io(&mut socket)?;
//			println!("handshake, read {_i} bytes, wrote {_o} bytes");
		} else if tls_client.wants_write() {
			let _o = tls_client.write_tls(&mut socket)?;
//			println!("wrote {_o} bytes");
		} else if tls_client.wants_read() {
			let _i = tls_client.read_tls(&mut socket)?;
//			println!("read {_i} TLS bytes");

			if tls_client.process_new_packets()?.plaintext_bytes_to_read() == 0 {
				continue;
			}

			let len = tls_client.reader().read(&mut buffer)?;
//			println!("read {len} plain bytes");

			let responses = buffer[0..len]
				.split(|&x|x == b'\r' || x == b'\n')
				.filter(|&x|x.len() != 0);

			for response in responses {
				if cli.verbose > 0 {
					println!("{}", String::from_utf8_lossy(response));
				}

				match state {
					State::Unauthenticated => if response.starts_with(b"* OK") {
						let request = format!("A001 login {} {}\r\n", cli.username, cli.password);
						tls_client.writer().write(request.as_bytes())?;
						state = State::Authenticated;
					},
					State::Authenticated => if response.starts_with(b"A001 OK") {
						tls_client.writer().write(b"A002 select inbox\r\n")?;
						state = State::Inbox;
					} else if response.starts_with(b"A001") {
						return Err(anyhow!("The server rejected authentication"));
					},
					State::Inbox => if response.starts_with(b"A002 OK") {
						tls_client.writer().write(b"A003 idle\r\n")?;
						state = State::Idling;
					} else if response.starts_with(b"A002") {
						return Err(anyhow!("Selecting inbox failed"));
					},
					State::Idling => if response.starts_with(b"+ idling") {
						println!("Connected and idling ...");
					} else if response.starts_with(b"*") && response.ends_with(b"EXISTS") {
						println!("NEW EMAIL!");
						Command::new(cli.command.as_os_str())
							.output()?;
					}
				}
			}
		} else {
			break;
		}

	}

	Ok(())
}
