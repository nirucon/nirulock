use std::{
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event},
};

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires isolated X server, font and xdotool"]
fn timeout_cannot_unlock_and_rejected_input_stays_isolated() {
    let directory = std::env::temp_dir().join(format!("nirulock-x11-{}", std::process::id()));
    std::fs::create_dir_all(directory.join("config/niruui")).unwrap();
    std::fs::create_dir_all(directory.join("state/nirulock")).unwrap();
    std::fs::write(
        directory.join("config/niruui/lock.toml"),
        "lock_timeout_minutes = 1\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("state/nirulock/trace.log"),
        "old key code 38 state 0\n",
    )
    .unwrap();
    let (connection, screen) = x11rb::connect(None).unwrap();
    let root = connection.setup().roots[screen].root;
    let spy = connection.generate_id().unwrap();
    connection
        .create_window(
            0,
            spy,
            root,
            10,
            10,
            200,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS),
        )
        .unwrap();
    connection.map_window(spy).unwrap();
    connection
        .set_input_focus(InputFocus::POINTER_ROOT, spy, x11rb::CURRENT_TIME)
        .unwrap();
    connection.flush().unwrap();
    let mut lock = Running(
        Command::new(env!("CARGO_BIN_EXE_nirulock"))
            .env("USER", "nirulock-no-such-user")
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_STATE_HOME", directory.join("state"))
            .env("NIRULOCK_SAFE_SECONDS", "1")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    thread::sleep(Duration::from_secs(2));
    assert!(
        lock.0.try_wait().unwrap().is_none(),
        "legacy environment bypass unlocked"
    );
    assert!(Command::new("xdotool")
        .args(["type", "wrong-password"])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("xdotool")
        .args(["key", "Return"])
        .status()
        .unwrap()
        .success());
    thread::sleep(Duration::from_secs(3));
    assert!(Command::new("xdotool")
        .args(["type", "after-rejection"])
        .status()
        .unwrap()
        .success());
    thread::sleep(Duration::from_secs(7));
    assert!(
        lock.0.try_wait().unwrap().is_none(),
        "legacy config timer unlocked"
    );
    connection.get_input_focus().unwrap().reply().unwrap();
    while let Some(event) = connection.poll_for_event().unwrap() {
        assert!(
            !matches!(event, Event::KeyPress(_)),
            "desktop client received lock input"
        );
    }
    let trace = std::fs::read_to_string(directory.join("state/nirulock/trace.log")).unwrap();
    assert!(!trace.contains("key code"));
    assert!(!trace.contains("wrong-password"));
    assert!(!trace.contains("after-rejection"));
    drop(lock);
    connection.destroy_window(spy).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
