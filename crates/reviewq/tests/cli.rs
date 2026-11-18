use std::process::Command;

#[test]
fn prints_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_reviewq"))
        .arg("--version")
        .output()
        .expect("launch the compiled reviewq binary");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        concat!("reviewq ", env!("CARGO_PKG_VERSION"), "\n").as_bytes()
    );
}
