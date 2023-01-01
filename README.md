# imapidle - run command on new email #

This is a very simple program which connects to an [IMAP] server and waits for email to arrive, using the `IDLE` command. Every time a new email arrives it runs a command. This can be useful to trigger a refresh in email clients that don't support `IDLE` themselves.

Although this is quite minimal and only implements a tiny subset of the IMAP protocol, it is supposed to be robust with respect to connection errors. The idea is that this gets started once at system startup and then survives bad wifi, suspending the machine, etc.

## Installation ##

Get [rust] and run

    cargo build --release

which creates the binary at the path `target/release/imapidle`.

## Usage ##

The output of `imapidle --help` does a good job at explaining how to use it:

    $ imapidle --help
    Uses IMAP IDLE to run a command whenever a new email arrives

    Usage: imapidle [OPTIONS] --server <SERVER> --username <USERNAME> --password <PASSWORD> --command <COMMAND>

    Options:
      -s, --server <SERVER>      IMAP server domain
      --port <PORT>              IMAP server port [default: 993]
      -u, --username <USERNAME>  IMAP user name
      -p, --password <PASSWORD>  IMAP password
      -i, --interval <INTERVAL>  interval (in seconds) at which to run even if no email arrives
      -c, --command <COMMAND>    command to run when new mail arrives
      -v, --verbose...           show all server responses
      -h, --help                 Print help information

Note that it only supports TLS encrypted IMAP and plain password authentication. Also, it currently reads the password from the command line, which isn't a great thing to do. I might change that eventually.

## Why? ##

Rust might not be the canonical programming language to do something like this in. And also, this probably already exists somewhere in a more complete and polished form. So this might not useful for anyone else. But I had three goals in making this:

1. I wanted my emails to arrive faster and without having to manually hit the refresh button.
2. I wanted to find out how IMAP works and why it's often so slow (I'm still not sure about the latter).
3. I wanted to try using rust for something practical and see how well it works. The result is it worked, and I would do it again.

[Rust]: https://www.rust-lang.org/
[IMAP]: https://en.wikipedia.org/wiki/Internet_Message_Access_Protocol
