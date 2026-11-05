//! Internal check: verify one password against PAM without needing X.
//!
//! Built from the same module as the lock screen, so a CI test exercises the
//! real authentication path including the service fallback. The password is
//! read from the first line of standard input, never from the command line.
#[path = "../pam.rs"]
mod pam;

use std::io::BufRead;
use zeroize::Zeroizing;

fn main() {
    let user = std::env::args()
        .nth(1)
        .expect("usage: pamcheck USER < password");
    let mut password = Zeroizing::new(String::with_capacity(4096));
    if std::io::stdin().lock().read_line(&mut password).is_err() {
        println!("ERROR cannot read the password from stdin");
        std::process::exit(2)
    }
    while password.ends_with('\n') || password.ends_with('\r') {
        password.pop();
    }
    match pam::authenticate(&user, &password) {
        Ok(outcome) if outcome.accepted => {
            println!("AUTH OK");
            std::process::exit(0)
        }
        Ok(outcome) => {
            match outcome.message {
                Some(message) => println!("REJECTED: {message}"),
                None => println!("REJECTED"),
            }
            std::process::exit(1)
        }
        Err(e) => {
            println!("ERROR {e}");
            std::process::exit(2)
        }
    }
}
