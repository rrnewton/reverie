use std::process::Command;

fn main() {
    let output = std::env::var("OUT_DIR").unwrap();
    let object = format!("{output}/window.o");
    assert!(
        Command::new("cc")
            .args([
                "-O2", "-g", "-Wall", "-Wextra", "-Werror", "-fPIC", "-c", "window.c", "-o",
                &object,
            ])
            .status()
            .unwrap()
            .success()
    );
    println!("cargo:rustc-link-arg={object}");
    println!("cargo:rerun-if-changed=window.c");
}
