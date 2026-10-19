use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{randr::ConnectionExt as _, xproto::*, Event},
};

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn lock_window(c: &x11rb::rust_connection::RustConnection, root: Window) -> Window {
    let name = c
        .intern_atom(false, b"_NET_WM_NAME")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        for window in c.query_tree(root).unwrap().reply().unwrap().children {
            let title = c
                .get_property(false, window, name, AtomEnum::ANY, 0, 128)
                .unwrap()
                .reply()
                .unwrap();
            if title.value == b"NIRULOCK"
                && c.get_window_attributes(window)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .map_state
                    == MapState::VIEWABLE
            {
                return window;
            }
        }
        assert!(Instant::now() < until, "lock never became ready");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires isolated X server, font and xdotool"]
fn later_overlays_cannot_cover_the_lock() {
    let (c, screen) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[screen].root;
    c.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
    )
    .unwrap()
    .check()
    .unwrap();
    let _lock = Running(
        Command::new(env!("CARGO_BIN_EXE_nirulock"))
            .env("USER", "nirulock-no-such-user")
            .spawn()
            .unwrap(),
    );
    let lock = lock_window(&c, root);
    // Readiness is explicit and names the lock window; it is not inferred
    // from a process id.
    let ready_atom = c
        .intern_atom(false, b"_NIRU_LOCK_READY")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let ready = c
        .get_property(false, root, ready_atom, AtomEnum::WINDOW, 0, 1)
        .unwrap()
        .reply()
        .unwrap()
        .value32()
        .and_then(|mut v| v.next());
    assert_eq!(ready, Some(lock), "the lock did not publish readiness");
    let overlay = c.generate_id().unwrap();
    c.create_window(
        0,
        overlay,
        root,
        10,
        10,
        320,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(0xff00ff),
    )
    .unwrap();
    c.map_window(overlay).unwrap();
    c.flush().unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let stack = c.query_tree(root).unwrap().reply().unwrap().children;
        if stack.last() == Some(&lock) {
            break;
        }
        assert!(
            Instant::now() < until,
            "newly mapped content remained above the lock"
        );
        thread::sleep(Duration::from_millis(20));
    }
    // Raising an already mapped overlay must also be repaired while PAM is
    // checking a password; authentication must not block the X event loop.
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
    thread::sleep(Duration::from_millis(100));
    c.configure_window(
        overlay,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )
    .unwrap();
    c.flush().unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    loop {
        if c.query_tree(root).unwrap().reply().unwrap().children.last() == Some(&lock) {
            break;
        }
        assert!(
            Instant::now() < until,
            "raised content remained above the lock"
        );
        thread::sleep(Duration::from_millis(20));
    }
    // A restack must settle, not keep responding to its own ConfigureNotify.
    while c.poll_for_event().unwrap().is_some() {}
    thread::sleep(Duration::from_millis(200));
    c.get_input_focus().unwrap().reply().unwrap();
    let mut reconfigurations = 0;
    while let Some(event) = c.poll_for_event().unwrap() {
        if matches!(event, Event::ConfigureNotify(e) if e.window == lock) {
            reconfigurations += 1;
            assert!(
                reconfigurations <= 4,
                "lock generated a self-triggered restack storm"
            );
        }
    }
}

#[test]
#[ignore = "requires isolated resizable RandR X server and font"]
fn root_resize_expands_opaque_lock_coverage() {
    let (c, screen) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[screen].root;
    let original = c.get_geometry(root).unwrap().reply().unwrap();
    assert!(
        original.width > 800 && original.height > 600,
        "use Xvfb's default 1280x1024 screen"
    );
    let resources = c
        .randr_get_screen_resources_current(root)
        .unwrap()
        .reply()
        .unwrap();
    let crtcs: Vec<_> = resources
        .crtcs
        .iter()
        .map(|id| {
            (
                *id,
                c.randr_get_crtc_info(*id, resources.config_timestamp)
                    .unwrap()
                    .reply()
                    .unwrap(),
            )
        })
        .collect();
    struct Restore<'a>(
        &'a x11rb::rust_connection::RustConnection,
        Window,
        u16,
        u16,
        Vec<(u32, x11rb::protocol::randr::GetCrtcInfoReply)>,
    );
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = self
                .0
                .randr_set_screen_size(self.1, self.2, self.3, 320, 240);
            for (id, info) in &self.4 {
                let current = self
                    .0
                    .randr_get_screen_resources_current(self.1)
                    .unwrap()
                    .reply()
                    .unwrap();
                let _ = self
                    .0
                    .randr_set_crtc_config(
                        *id,
                        x11rb::CURRENT_TIME,
                        current.config_timestamp,
                        info.x,
                        info.y,
                        info.mode,
                        info.rotation,
                        &info.outputs,
                    )
                    .unwrap()
                    .reply();
            }
            let _ = self.0.flush();
        }
    }
    let restore = Restore(&c, root, original.width, original.height, crtcs);
    // Xvfb has one fixed output mode. Disable its CRTC while changing root size;
    // no synthetic ConfigureNotify is used to simulate the resize regression.
    for (id, _) in &restore.4 {
        let current = c
            .randr_get_screen_resources_current(root)
            .unwrap()
            .reply()
            .unwrap();
        let reply = c
            .randr_set_crtc_config(
                *id,
                x11rb::CURRENT_TIME,
                current.config_timestamp,
                0,
                0,
                0,
                x11rb::protocol::randr::Rotation::ROTATE0,
                &[],
            )
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(reply.status, x11rb::protocol::randr::SetConfig::SUCCESS);
    }
    c.randr_set_screen_size(root, 800, 600, 200, 150)
        .unwrap()
        .check()
        .unwrap();
    let mut lock = Running(
        Command::new(env!("CARGO_BIN_EXE_nirulock"))
            .spawn()
            .unwrap(),
    );
    let window = lock_window(&c, root);
    assert_eq!(c.get_geometry(window).unwrap().reply().unwrap().width, 800);
    c.randr_set_screen_size(root, original.width, original.height, 320, 240)
        .unwrap()
        .check()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(
            lock.0.try_wait().unwrap().is_none(),
            "resize dismissed the lock"
        );
        let geometry = c.get_geometry(window).unwrap().reply().unwrap();
        if (geometry.width, geometry.height) == (original.width, original.height) {
            break;
        }
        assert!(
            Instant::now() < until,
            "lock did not cover the expanded root"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let corner = c
        .get_image(
            ImageFormat::Z_PIXMAP,
            window,
            (original.width - 1) as i16,
            (original.height - 1) as i16,
            1,
            1,
            u32::MAX,
        )
        .unwrap()
        .reply()
        .unwrap();
    assert!(
        corner.data.iter().all(|v| *v == 0),
        "expanded corner was not opaque black"
    );
}

#[test]
#[ignore = "requires isolated X server and font"]
fn failed_keyboard_grab_never_displays_a_password_prompt() {
    let (connection, screen) = x11rb::connect(None).unwrap();
    let root = connection.setup().roots[screen].root;
    assert_eq!(
        connection
            .grab_keyboard(
                false,
                root,
                x11rb::CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC
            )
            .unwrap()
            .reply()
            .unwrap()
            .status,
        GrabStatus::SUCCESS
    );
    let name = connection
        .intern_atom(false, b"_NET_WM_NAME")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let mut lock = Running(
        Command::new(env!("CARGO_BIN_EXE_nirulock"))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    thread::sleep(Duration::from_millis(400));
    for window in connection
        .query_tree(root)
        .unwrap()
        .reply()
        .unwrap()
        .children
    {
        let title = connection
            .get_property(false, window, name, AtomEnum::ANY, 0, 128)
            .unwrap()
            .reply()
            .unwrap();
        if title.value == b"NIRULOCK" {
            assert_eq!(
                connection
                    .get_window_attributes(window)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .map_state,
                MapState::UNMAPPED,
                "password prompt became visible before keyboard was acquired"
            );
        }
    }
    thread::sleep(Duration::from_secs(2));
    assert!(!lock
        .0
        .try_wait()
        .unwrap()
        .expect("failed grab did not terminate startup")
        .success());
    connection.ungrab_keyboard(x11rb::CURRENT_TIME).unwrap();
    connection.flush().unwrap();
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
