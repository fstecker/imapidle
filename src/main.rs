#![feature(io_error_more)]

use anyhow::Result as AResult;
use std::io::{Error as IOError, ErrorKind};
use std::thread;
use std::time::Duration;
use clap::Parser;

const CONNECTION_ERRORS: &[ErrorKind] = &[
	ErrorKind::ConnectionAborted,
	ErrorKind::ConnectionReset,
	ErrorKind::NetworkUnreachable,
	ErrorKind::HostUnreachable
];

fn main() -> AResult<()> {
	let cli = imapidle::Cli::parse();

	loop {
		return match imapidle::run(&cli) {
			Ok(_) => Ok(()),
			Err(err) => match err.downcast_ref::<IOError>() {
				Some(io_err) if io_err.kind() == ErrorKind::WouldBlock => {
					let secs_to_reconnect = 10;
					println!("Timed out, reconnecting in {secs_to_reconnect} seconds");
					thread::sleep(Duration::from_secs(secs_to_reconnect));
					continue;
				},
				Some(io_err) if CONNECTION_ERRORS.contains(&io_err.kind()) => {
					let secs_to_reconnect = 10*60;
					println!("Cannot connect currently, retrying in {secs_to_reconnect} seconds");
					thread::sleep(Duration::from_secs(secs_to_reconnect));
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
