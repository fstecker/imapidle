#![feature(io_error_more)]

use rustls::{OwnedTrustAnchor, ClientConfig, RootCertStore, ClientConnection};
use anyhow::{Result as AResult, bail};
use std::net::{TcpStream, ToSocketAddrs, SocketAddr};
use std::io::{self, ErrorKind, Read, Write, Error as IOError};
use std::process::Command;
use std::time::{Duration, SystemTime};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::cell::RefCell;
use std::thread;
use clap::Parser;

#[derive(Parser, Clone)]
#[command(about = "Uses IMAP IDLE to run a command whenever a new email arrives", long_about = None)]
pub struct Cli {
	/// IMAP server domain
	#[arg(short, long)]
	server: String,

	/// IMAP server port
	#[arg(long, default_value_t = 993)]
	port: u16,

	// the resolved address(es)
	#[arg(skip)]
	addrs: RefCell<Vec<SocketAddr>>,

	/// IMAP user name
	#[arg(short, long)]
	username: String,

	/// IMAP password
	#[arg(short, long)]
	password: String,

	/// interval (in seconds) at which to run even if no email arrives
	#[arg(short, long)]
	interval: Option<u64>,

	/// command to run when new mail arrives
	#[arg(short, long)]
	command: PathBuf,

	/// show all server responses
	#[arg(short, long, action = clap::ArgAction::Count)]
	verbose: u8,
}

#[derive(Debug)]
struct Status {
	connected: bool,
	last_run: SystemTime,
}

const CONNECTION_LOST_ERRORS: &[ErrorKind] = &[
	ErrorKind::Interrupted,
	ErrorKind::WouldBlock,      // when a read times out
];

const CANT_CONNECT_ERRORS: &[ErrorKind] = &[
	ErrorKind::ConnectionAborted,
	ErrorKind::ConnectionReset,
	ErrorKind::NotConnected,
	ErrorKind::NetworkUnreachable,
	ErrorKind::HostUnreachable,
];

pub fn run() -> AResult<()> {
	let cli = Cli::parse();

	let connection_status: Arc<Mutex<Status>> = Arc::new(Mutex::new(
		Status { connected: false, last_run: SystemTime::now() }
	));

	// if interval is given, spawn a thread to take care of the regular calls
	let timer_handle = cli.interval.map(|interval| {
		let cmd = cli.command.clone();
		let connection_status_subthread = Arc::clone(&connection_status);
		thread::spawn(move || {
			let interval_duration = Duration::from_secs(interval);
			let mut wait_time = interval_duration;

			loop {
				// we just want to sleep, but park_timeout() allows
				// interruption by the main thread
				thread::park_timeout(wait_time);

				let mut status = connection_status_subthread.lock().unwrap();

				// check here if we should really run, and how long to wait for if not
				// interval, SystemTime::now(), status.connected, status.last_run -> wait_time
				let elapsed = status.last_run.elapsed().unwrap_or_default();
				let run = status.connected && elapsed >= interval_duration;

				// println!("time = {:?}, run = {}", SystemTime::now(), run);

				if run {
					println!("Interval timer expired, running command ...");

					Command::new(cmd.as_os_str())
						.output()
						.expect("command execution failed");

					println!("Command finished.");

					status.last_run = SystemTime::now();

					wait_time = interval_duration;
				} else {
					// wait for the remaining time until interval_duration
					// if the time is already up, that means we're not connected currently
					// in that case, just wait 1/2 hour, once the connection is reestablished
					// the main thread will unpark us anyway
					wait_time = interval_duration.checked_sub(elapsed)
						.unwrap_or(Duration::from_secs(1800));
				}
			}
		})
	});

	// what to do as soon as we're connected
	let connect_callback = || {
		connection_status.lock().unwrap().connected = true;

		// we unpark the thread after reconnecting since a common cause of
		// disconnects is suspend, after which the sleep timer might not do what
		// we want
		if let Some(th) = &timer_handle {
			th.thread().unpark();
		}
	};

	// what to do when the server tells us we got an email
	let mail_callback = || {
		let mut status = connection_status.lock().unwrap();

		println!("New email, running command ...");

		Command::new(cli.command.as_os_str())
			.output()
			.expect("command execution failed");

		println!("Command finished.");

		status.last_run = SystemTime::now();
	};

	// reconnect in an infinite loop, with exponentially increasing wait times up to 1/2 hour
	let mut time_to_reconnect: u64 = 1;
	loop {
		return match connect_and_idle(&cli, connect_callback , mail_callback) {
			Ok(_) => Ok(()),
			Err(err) => match err.downcast_ref::<IOError>() {
				Some(io_err) if CONNECTION_LOST_ERRORS.contains(&io_err.kind()) => {
					connection_status.lock().unwrap().connected = false;

					time_to_reconnect = 1;
					println!("Connection lost, reconnecting in {time_to_reconnect} seconds");
					thread::sleep(Duration::from_secs(time_to_reconnect));

					continue;
				},
				Some(io_err) if CANT_CONNECT_ERRORS.contains(&io_err.kind()) => {
					connection_status.lock().unwrap().connected = false;

					time_to_reconnect = u64::min(time_to_reconnect*2, 1800);

					if cli.verbose > 0 {
						println!("Error: {:?}", err);
					}
					println!("Cannot connect currently, retrying in {time_to_reconnect} seconds");

					thread::sleep(Duration::from_secs(time_to_reconnect));

					continue;
				},
				Some(io_err) => {
					println!("{:?}", io_err.kind());
					Err(err)
				}
				_ => Err(err)
			}
		}
	}
}

#[derive(PartialEq, Eq, Debug)]
enum ImapState {
	Unauthenticated,
	Authenticated,
	Inbox,
	Idling
}

/// establish a connection to IMAP server, log in, run IDLE command, and wait
/// for mail to arrive
pub fn connect_and_idle<F: Fn(), G: Fn()>(cli: &Cli, connected_callback: F, mail_callback: G) -> AResult<()> {
	let tls_config = ClientConfig::builder()
		.with_safe_defaults()
		.with_root_certificates(RootCertStore {
			roots: webpki_roots::TLS_SERVER_ROOTS.0.iter()
				.map(|ta| OwnedTrustAnchor::from_subject_spki_name_constraints(
					ta.subject, ta.spki, ta.name_constraints))
				.collect()
		})
		.with_no_client_auth();

	let mut buffer = [0u8; 2048];

	let mut tls_client = ClientConnection::new(
		Arc::new(tls_config),
		cli.server.as_str().try_into().unwrap())?;

	let mut addrs = cli.addrs.borrow_mut();
	if addrs.is_empty() {
		addrs.extend(
			(cli.server.as_str(), cli.port)
				.to_socket_addrs()
				.map_err(|e|io::Error::new(ErrorKind::NotConnected, e.to_string()))?
		);
	}

	let mut socket = TcpStream::connect(addrs.as_slice())?;
	let mut state = ImapState::Unauthenticated;

	socket.set_read_timeout(Some(Duration::from_secs(120)))?;

	loop {
		if tls_client.is_handshaking() {
			let (_i, _o) = tls_client.complete_io(&mut socket)?;
		} else if tls_client.wants_write() {
			let _o = tls_client.write_tls(&mut socket)?;
		} else if tls_client.wants_read() {
			let _i = tls_client.read_tls(&mut socket)?;

			if tls_client.process_new_packets()?.plaintext_bytes_to_read() == 0 {
				continue;
			}

			let len = tls_client.reader().read(&mut buffer)?;

			let responses = buffer[0..len]
				.split(|&x|x == b'\r' || x == b'\n')
				.filter(|&x|x.len() != 0);

			for response in responses {
				if cli.verbose > 0 {
					if state == ImapState::Unauthenticated {
						if let Some(suite) = tls_client.negotiated_cipher_suite() {
							println!("negotiated cipher suite: {:?}", suite);
						}
					}

					println!("{}", String::from_utf8_lossy(response));
				}

				match state {
					ImapState::Unauthenticated => if response.starts_with(b"* OK") {
						let request = format!("A001 login {} {}\r\n", cli.username, cli.password);
						tls_client.writer().write(request.as_bytes())?;
						state = ImapState::Authenticated;
					},
					ImapState::Authenticated => if response.starts_with(b"A001 OK") {
						tls_client.writer().write(b"A002 select inbox\r\n")?;
						state = ImapState::Inbox;
					} else if response.starts_with(b"A001") {
						bail!("The server rejected authentication");
					},
					ImapState::Inbox => if response.starts_with(b"A002 OK") {
						tls_client.writer().write(b"A003 idle\r\n")?;
						state = ImapState::Idling;
						connected_callback();
						// notify timer thread that we're live
					} else if response.starts_with(b"A002") {
						bail!("Selecting inbox failed");
					},
					ImapState::Idling => if response.starts_with(b"+ idling") {
						println!("Connected and idling ...");
					} else if response.starts_with(b"*") && response.ends_with(b"EXISTS") {
						mail_callback();
					}
				}
			}
		} else {
			// if wants_read() and wants_write() are both false, this usually means the connection was closed
			// so just return "Interrupted" and reconnect after a few seconds
			return Err(io::Error::new(ErrorKind::Interrupted, "Connection was closed by server").into());
		}
	}
}
