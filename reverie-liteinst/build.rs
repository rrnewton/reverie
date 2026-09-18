use std::path::PathBuf;

fn main() {
    let script =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("liteinst-helper.ld");
    println!("cargo:rerun-if-changed={}", script.display());
    let require_runtime = concat!(
        "-Wl,--undefined=reverie_liteinst_host_install_helper,",
        "--defsym=__reverie_liteinst_requires_runtime_helper=1"
    );
    println!("cargo:rustc-link-arg-cdylib={require_runtime}");
    for binary in [
        "reverie-liteinst-strace",
        "reverie-liteinst-lifecycle-guest",
        "reverie-liteinst-rpc-tool-guest",
    ] {
        println!("cargo:rustc-link-arg-bin={binary}={require_runtime}");
    }
    let link_arg = format!("-Wl,-T,{}", script.display());
    println!("cargo:rustc-link-arg={link_arg}");
}
