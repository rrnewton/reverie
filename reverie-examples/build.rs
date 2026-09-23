use std::path::PathBuf;

fn main() {
    let script = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../reverie-liteinst/liteinst-helper.ld");
    println!("cargo:rerun-if-changed={}", script.display());
    let require_runtime = concat!(
        "-Wl,--undefined=reverie_liteinst_host_install_helper,",
        "--defsym=__reverie_liteinst_downstream_requires_runtime_helper=1"
    );
    println!("cargo:rustc-link-arg-cdylib={require_runtime}");
    println!("cargo:rustc-link-arg=-Wl,-T,{}", script.display());
}
