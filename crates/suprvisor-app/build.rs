// Links the icon and version resources (res/app.res, made by res/make-icon.py) into the exe.
fn main() {
    let res = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("res").join("app.res");
    println!("cargo:rerun-if-changed={}", res.display());
    println!("cargo:rustc-link-arg-bins={}", res.display());
}
