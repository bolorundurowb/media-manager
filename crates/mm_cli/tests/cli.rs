use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn organize_without_yes_does_not_apply() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("Inception.2010.mkv"), b"bytes").unwrap();

    let mut cmd = Command::cargo_bin("media-manager").unwrap();
    let out = cmd
        .args(["organize", dir.path().to_str().unwrap(), "--type", "movies"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(
        stdout.contains("MOVE") || stdout.contains("Plan for"),
        "expected plan text, got {stdout}"
    );
    assert!(dir.path().join("Inception.2010.mkv").exists());
}

#[test]
fn organize_dry_run_prints_plan() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("Inception.2010.mkv"), b"bytes").unwrap();

    Command::cargo_bin("media-manager")
        .unwrap()
        .args([
            "organize",
            dir.path().to_str().unwrap(),
            "--type",
            "movies",
            "--dry-run",
        ])
        .assert()
        .success();
    assert!(dir.path().join("Inception.2010.mkv").exists());
}
