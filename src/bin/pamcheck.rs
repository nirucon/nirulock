//! Internal check: verify one password against PAM without needing X.
//!
//! Built from the same module as the lock screen, so a CI test exercises the
//! real authentication path including the service fallback.
#[path = "../pam.rs"]
mod pam;

fn main() {
    let mut args = std::env::args().skip(1);
    let user = args.next().expect("usage: pamcheck USER PASSWORD");
    let password = args.next().expect("usage: pamcheck USER PASSWORD");
    match pam::authenticate(&user, &password) {
        Ok(true) => {
            println!("AUTH OK");
            std::process::exit(0)
        }
        Ok(false) => {
            println!("REJECTED");
            std::process::exit(1)
        }
        Err(e) => {
            println!("ERROR {e}");
            std::process::exit(2)
        }
    }
}
