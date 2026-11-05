//! Runs the isolated Linux SCM_RIGHTS and flock research fixture.
#![cfg(target_os = "linux")]

#[test]
fn transferred_lock_fixture() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fd_gate_fixture.py"
        ))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("python3 is required for the Linux gate research fixture");
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
