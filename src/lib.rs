use rustls::{OwnedTrustAnchor, ClientConfig, RootCertStore, ClientConnection};
use anyhow::{Result as AResult, bail};
use std::net::{TcpStream, ToSocketAddrs};
use std::io::{self, ErrorKind, Read, Write};
use std::process::Command;
use std::time::{Duration, SystemTime};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::mem;
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

#[derive(PartialEq, Eq, Debug)]
enum State {
	Unauthenticated,
	Authenticated,
	Inbox,
	Idling
}

/// runs the command if one of the following conditions is satisfied:
/// - `new_mail` is true
/// - the command hasn't been run yet
/// - `cli.interval` is set and the last run was at least `cli.interval` minus 1 second ago
///
/// The return value is the duration to wait until command should be run again
/// (assuming no new mail arrives in the meantime). This is `cli.interval` if
/// the command was run, and `cli.interval` minus the time elapsed since the last run
/// in case the command was not run.
/// If `cli.interval` is `None`, the return value will be `None`.

pub fn run_command_if_needed(cli: &Cli, lastrun_mutex: &Mutex<Option<SystemTime>>, new_mail: bool) -> Option<Duration> {
	// locks the mutex until the end of the function, also preventing that the command is run concurrently
	let mut last = lastrun_mutex.lock().unwrap();
	let run;

	if new_mail {
		run = true;
	} else {
		match *last {
			None => {
				run = true;
			},
			Some(last_inner) => {
				match cli.interval {
					None => {
						run = false;
					},
					Some(int) => {
						let min_duration = Duration::from_secs(int - 1);
						let duration = last_inner.elapsed().unwrap_or(Duration::ZERO);
						if duration > min_duration {
							run = true;
						} else {
							run = false;
						}
					}
				}
			}
		}
	}

	if run {
		println!("Running command ...");
		Command::new(cli.command.as_os_str())
			.output()
			.expect("command execution failed");
		println!("Command finished.");

		*last = Some(SystemTime::now());
	}

	// subtract the time already elapsed since last run
	return cli.interval.map(|int| {
		Duration::from_secs(int).saturating_sub(
			last
			.expect("command should have run at least once")
			.elapsed()
			.unwrap_or(Duration::ZERO))
	});
}

pub fn run(cli: &Cli) -> AResult<()> {
	// a mutex to avoid running the command concurrently
	let mutex_mainthread = Arc::new(Mutex::new(()));
	let mutex_subthread = Arc::clone(&mutex_mainthread);

	if let Some(interval) = cli.interval {
		let cmd = cli.command.clone();
		thread::spawn(move || {
			loop {
				thread::sleep(Duration::from_secs(interval));
				let lock = mutex_subthread.lock().unwrap();
				println!("Interval timer expired, running command ...");
				Command::new(cmd.as_os_str())
					.output()
					.expect("command execution failed");
				println!("Command finished.");
				mem::drop(lock);
			}
		});
	}

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
	let addrs = (cli.server.as_str(), cli.port)
		.to_socket_addrs()
		.map_err(|e|io::Error::new(ErrorKind::NotConnected, e.to_string()))?
		.collect::<Vec<_>>();
	let mut socket = TcpStream::connect(addrs.as_slice())?;
	let mut state = State::Unauthenticated;

	socket.set_read_timeout(Some(Duration::from_secs(10*60)))?;

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
					if state == State::Unauthenticated {
						if let Some(suite) = tls_client.negotiated_cipher_suite() {
							println!("negotiated cipher suite: {:?}", suite);
						}
					}

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
						bail!("The server rejected authentication");
					},
					State::Inbox => if response.starts_with(b"A002 OK") {
						tls_client.writer().write(b"A003 idle\r\n")?;
						state = State::Idling;
					} else if response.starts_with(b"A002") {
						bail!("Selecting inbox failed");
					},
					State::Idling => if response.starts_with(b"+ idling") {
						println!("Connected and idling ...");
					} else if response.starts_with(b"*") && response.ends_with(b"EXISTS") {
						let lock = mutex_mainthread.lock().unwrap();
						println!("New email, running command ...");
						Command::new(cli.command.as_os_str())
							.output()
							.expect("command execution failed");
						println!("Command finished.");
						mem::drop(lock);
					}
				}
			}
		} else {
			break;
		}
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_dns_lookup() {

	}
}
