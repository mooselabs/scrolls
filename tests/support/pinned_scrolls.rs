use std::{path::Path, process::Command};

pub fn build_0_3_0() -> tempfile::TempDir {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(".verification/scrolls-0.3.0");
    build_pinned(&source, "3ae0a38dcc4da0a508e82ce4737d226e06d6f2b4")
}

pub fn build_pinned(source: &Path, revision: &str) -> tempfile::TempDir {
    let pin = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("pinned baseline checkout required, see docs/shared-authoring.md");
    assert!(pin.status.success(), "pinned baseline checkout required");
    assert_eq!(String::from_utf8_lossy(&pin.stdout).trim(), revision);
    let clean = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        clean.status.success() && clean.stdout.is_empty(),
        "baseline source must be clean"
    );
    let build_dir = tempfile::tempdir().unwrap();
    let target = build_dir.path();
    let build = Command::new("cargo")
        .args(["build", "--locked", "--bin", "scrolls"])
        .current_dir(source)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    build_dir
}
