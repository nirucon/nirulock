// Link libpam. See src/pam.rs for why NIRULOCK is the one dynamic NIRU binary.
//
// The system libraries are asked for through pkg-config where available and
// fall back to the usual library names, so this builds on Arch, Debian and
// Void without extra configuration. Users never need this: releases are
// built in CI and the result runs on any glibc system.
fn main() {
    if pkg_config("pam") {
        return;
    }
    // No .pc file (some minimal images): ask the linker directly.
    println!("cargo:rustc-link-lib=pam");
    println!("cargo:rerun-if-changed=build.rs");
}

fn pkg_config(lib: &str) -> bool {
    let Ok(out) = std::process::Command::new("pkg-config")
        .args(["--libs", lib])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    for flag in String::from_utf8_lossy(&out.stdout).split_whitespace() {
        if let Some(path) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
        } else if let Some(name) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={name}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    true
}
