fn main() {
    println!("cargo:rerun-if-changed=src/shim.c");
    cc::Build::new().file("src/shim.c").warnings(true).compile("reelforge_ofx_shim");
}
