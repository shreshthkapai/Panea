use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"PANEA01\0";

fn main() {
    println!("cargo:rerun-if-env-changed=PANEA_PACKAGE_ROOT");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let destination = output.join("payload.bin");
    let root = env::var_os("PANEA_PACKAGE_ROOT").map(PathBuf::from);
    if let Err(error) = write_payload(root.as_deref(), &destination) {
        panic!("failed to build installer payload: {error}");
    }
    compile_windows_resources();
}

#[cfg(windows)]
fn compile_windows_resources() {
    let icon = Path::new("../../crates/assets/branding/generated/panea.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    winres::WindowsResource::new()
        .set_icon(icon.to_string_lossy().as_ref())
        .set("ProductName", "Panea Installer")
        .set("FileDescription", "Panea Terminal Installer")
        .compile()
        .expect("compile Panea installer resources");
}

#[cfg(not(windows))]
fn compile_windows_resources() {}

fn write_payload(root: Option<&Path>, destination: &Path) -> io::Result<()> {
    let mut files = Vec::new();
    if let Some(root) = root.filter(|root| root.is_dir()) {
        collect_files(root, root, &mut files)?;
    }
    files.sort();

    let mut output = fs::File::create(destination)?;
    output.write_all(MAGIC)?;
    output.write_all(&u32::try_from(files.len()).unwrap_or(u32::MAX).to_le_bytes())?;
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let root = root.expect("payload root exists when files were collected");
        let relative = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&path)?;
        let relative = relative.as_bytes();
        output.write_all(
            &u32::try_from(relative.len())
                .map_err(io::Error::other)?
                .to_le_bytes(),
        )?;
        output.write_all(
            &u64::try_from(bytes.len())
                .map_err(io::Error::other)?
                .to_le_bytes(),
        )?;
        output.write_all(relative)?;
        output.write_all(&bytes)?;
    }
    Ok(())
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(root, &path, output)?;
        } else if path.is_file() && path.starts_with(root) {
            output.push(path);
        }
    }
    Ok(())
}
