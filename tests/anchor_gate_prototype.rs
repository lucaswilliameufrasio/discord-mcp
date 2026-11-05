//! Runs the isolated Linux descriptor-anchor experiment.
#![cfg(target_os = "linux")]

#[test]
fn descriptor_anchor_fixture() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/anchor_gate_fixture.py"
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
