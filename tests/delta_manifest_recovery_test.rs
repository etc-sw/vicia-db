#[test]
fn delta_manifest_recovery_unit_gate_passes() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .args([
            "test",
            "--lib",
            "storage::delta_manifest",
            "--",
            "--nocapture",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("nested cargo test should run");

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("internal delta manifest tests failed\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }
}
