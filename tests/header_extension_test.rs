#[test]
fn header_extension_unit_gate_passes() -> Result<(), Box<dyn std::error::Error>> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .args([
            "test",
            "--lib",
            "storage::header_extension",
            "--",
            "--nocapture",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message =
            format!("internal header extension tests failed\nstdout:\n{stdout}\nstderr:\n{stderr}");
        return Err(std::io::Error::other(message).into());
    }

    Ok(())
}
