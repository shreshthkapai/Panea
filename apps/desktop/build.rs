#[cfg(windows)]
fn main() {
    println!("cargo:rerun-if-changed=../../crates/assets/branding/generated/panea.ico");
    let mut resource = winres::WindowsResource::new();
    resource.set_icon("../../crates/assets/branding/generated/panea.ico");
    resource
        .compile()
        .expect("failed to embed the Panea application icon");
}

#[cfg(not(windows))]
fn main() {}
