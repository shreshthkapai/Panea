use std::{
    env, fs,
    io::{self, Cursor, Read},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

const PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));
const MAGIC: &[u8; 8] = b"PANEA01\0";

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Panea installer failed: {error}");
            ExitCode::from(1)
        }
    }
}

#[derive(Debug, Clone)]
struct InstallOptions {
    install_dir: PathBuf,
    shortcuts: bool,
    update_path: bool,
    register_uninstall: bool,
}

fn run(args: Vec<String>) -> Result<String, String> {
    let command = args.first().map_or("install", String::as_str);
    if matches!(command, "help" | "--help" | "-h") {
        return Ok(help_text().to_owned());
    }
    let option_args = if matches!(command, "install" | "uninstall") {
        &args[1..]
    } else {
        &args[..]
    };
    let options = parse_options(option_args)?;

    if command == "uninstall" {
        uninstall(&options)?;
        return Ok(format!(
            "Panea removed from {}",
            options.install_dir.display()
        ));
    }
    if command != "install" && command.starts_with('-') {
        return Err(format!("unknown option: {command}"));
    }
    install(&options)?;
    Ok(format!(
        "Panea installed to {}",
        options.install_dir.display()
    ))
}

fn help_text() -> &'static str {
    "Panea per-user installer\n\nUsage:\n  panea-installer.exe install [--install-dir PATH] [--no-shortcuts] [--no-path] [--no-register]\n  panea-installer.exe uninstall [--install-dir PATH] [--no-shortcuts] [--no-path] [--no-register]"
}

fn parse_options(args: &[String]) -> Result<InstallOptions, String> {
    let mut options = InstallOptions {
        install_dir: default_install_dir()?,
        shortcuts: true,
        update_path: true,
        register_uninstall: true,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--install-dir" => {
                index += 1;
                options.install_dir = args
                    .get(index)
                    .map(PathBuf::from)
                    .ok_or_else(|| "--install-dir requires a path".to_owned())?;
            }
            "--no-shortcuts" => options.shortcuts = false,
            "--no-path" => options.update_path = false,
            "--no-register" => options.register_uninstall = false,
            other => return Err(format!("unknown installer option: {other}")),
        }
        index += 1;
    }
    Ok(options)
}

fn default_install_dir() -> Result<PathBuf, String> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("Programs").join("Panea"))
        .ok_or_else(|| "LOCALAPPDATA is unavailable; pass --install-dir".to_owned())
}

fn install(options: &InstallOptions) -> Result<(), String> {
    let entries = parse_payload(PAYLOAD)?;
    if entries.is_empty() {
        return Err(
            "installer contains no package payload; build it through cargo xtask package"
                .to_owned(),
        );
    }

    let parent = options
        .install_dir
        .parent()
        .ok_or_else(|| "install directory must have a parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create install parent: {error}"))?;
    let staging = parent.join(format!(".panea-install-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|error| format!("failed to clear installer staging directory: {error}"))?;
    }
    fs::create_dir_all(&staging)
        .map_err(|error| format!("failed to create installer staging directory: {error}"))?;

    for entry in entries {
        let destination = staging.join(&entry.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        fs::write(&destination, entry.bytes)
            .map_err(|error| format!("failed to write {}: {error}", destination.display()))?;
    }

    let backup = parent.join(format!(".panea-backup-{}", std::process::id()));
    if options.install_dir.exists() {
        fs::rename(&options.install_dir, &backup)
            .map_err(|error| format!("failed to prepare existing Panea upgrade: {error}"))?;
    }
    if let Err(error) = fs::rename(&staging, &options.install_dir) {
        if backup.exists() {
            let _ = fs::rename(&backup, &options.install_dir);
        }
        return Err(format!("failed to activate Panea installation: {error}"));
    }
    if backup.exists() {
        fs::remove_dir_all(&backup).map_err(|error| {
            format!("installed Panea but failed to remove old version: {error}")
        })?;
    }

    let installer = env::current_exe().map_err(|error| error.to_string())?;
    fs::copy(installer, options.install_dir.join("panea-uninstall.exe"))
        .map_err(|error| format!("failed to install uninstaller: {error}"))?;
    configure_windows_integration(options, true)?;
    Ok(())
}

fn uninstall(options: &InstallOptions) -> Result<(), String> {
    configure_windows_integration(options, false)?;
    if !options.install_dir.exists() {
        return Ok(());
    }
    let current = env::current_exe().map_err(|error| error.to_string())?;
    if current.starts_with(&options.install_dir) {
        let script = format!(
            "Start-Sleep -Milliseconds 300; Remove-Item -LiteralPath '{}' -Recurse -Force",
            powershell_quote(&options.install_dir.to_string_lossy())
        );
        Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .spawn()
            .map_err(|error| format!("failed to schedule uninstall cleanup: {error}"))?;
    } else {
        fs::remove_dir_all(&options.install_dir)
            .map_err(|error| format!("failed to remove Panea: {error}"))?;
    }
    Ok(())
}

fn configure_windows_integration(options: &InstallOptions, install: bool) -> Result<(), String> {
    if !cfg!(windows) {
        return Ok(());
    }
    let binary = options.install_dir.join("panea-gui.exe");
    let uninstall = options.install_dir.join("panea-uninstall.exe");
    let start_menu = env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Microsoft\\Windows\\Start Menu\\Programs\\Panea"));
    let mut script = String::new();
    let uninstall_registry =
        "HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Panea";
    if options.register_uninstall {
        if install {
            let uninstall = powershell_quote(&uninstall.to_string_lossy());
            let install_dir = powershell_quote(&options.install_dir.to_string_lossy());
            script.push_str(&format!(
                "$r='{uninstall_registry}';New-Item -Path $r -Force|Out-Null;New-ItemProperty -Path $r -Name DisplayName -Value 'Panea' -PropertyType String -Force|Out-Null;New-ItemProperty -Path $r -Name DisplayVersion -Value '{}' -PropertyType String -Force|Out-Null;New-ItemProperty -Path $r -Name InstallLocation -Value '{install_dir}' -PropertyType String -Force|Out-Null;New-ItemProperty -Path $r -Name UninstallString -Value ('\"{uninstall}\" uninstall') -PropertyType String -Force|Out-Null;",
                env!("CARGO_PKG_VERSION")
            ));
        } else {
            script.push_str(&format!(
                "Remove-Item -LiteralPath '{uninstall_registry}' -Recurse -Force -ErrorAction SilentlyContinue;"
            ));
        }
    }
    if options.update_path {
        let dir = powershell_quote(&options.install_dir.to_string_lossy());
        if install {
            script.push_str(&format!("$p=[Environment]::GetEnvironmentVariable('Path','User');$v=@($p -split ';' | Where-Object {{$_ -and $_ -ne '{dir}'}});$v+='{dir}';[Environment]::SetEnvironmentVariable('Path',($v -join ';'),'User');"));
        } else {
            script.push_str(&format!("$p=[Environment]::GetEnvironmentVariable('Path','User');$v=@($p -split ';' | Where-Object {{$_ -and $_ -ne '{dir}'}});[Environment]::SetEnvironmentVariable('Path',($v -join ';'),'User');"));
        }
    }
    if options.shortcuts
        && let Some(start_menu) = start_menu
    {
        let menu = powershell_quote(&start_menu.to_string_lossy());
        if install {
            let binary = powershell_quote(&binary.to_string_lossy());
            let uninstall = powershell_quote(&uninstall.to_string_lossy());
            script.push_str(&format!("New-Item -ItemType Directory -Force -Path '{menu}'|Out-Null;$w=New-Object -ComObject WScript.Shell;$s=$w.CreateShortcut('{menu}\\Panea.lnk');$s.TargetPath='{binary}';$s.WorkingDirectory='{}';$s.Save();$u=$w.CreateShortcut('{menu}\\Uninstall Panea.lnk');$u.TargetPath='{uninstall}';$u.Arguments='uninstall';$u.Save();", powershell_quote(&options.install_dir.to_string_lossy())));
        } else {
            script.push_str(&format!(
                "Remove-Item -LiteralPath '{menu}' -Recurse -Force -ErrorAction SilentlyContinue;"
            ));
        }
    }
    if script.is_empty() {
        return Ok(());
    }
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .status()
        .map_err(|error| format!("failed to run Windows integration: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Windows integration exited with {status}"))
    }
}

fn powershell_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[derive(Debug)]
struct PayloadEntry<'a> {
    path: PathBuf,
    bytes: &'a [u8],
}

fn parse_payload(payload: &[u8]) -> Result<Vec<PayloadEntry<'_>>, String> {
    let mut cursor = Cursor::new(payload);
    let mut magic = [0u8; 8];
    cursor.read_exact(&mut magic).map_err(io_error)?;
    if &magic != MAGIC {
        return Err("invalid installer payload signature".to_owned());
    }
    let count = read_u32(&mut cursor)?;
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let path_len = read_u32(&mut cursor)? as usize;
        let size = usize::try_from(read_u64(&mut cursor)?)
            .map_err(|_| "installer payload entry is too large".to_owned())?;
        let position = usize::try_from(cursor.position()).map_err(|error| error.to_string())?;
        let path_end = position
            .checked_add(path_len)
            .ok_or_else(|| "installer path length overflow".to_owned())?;
        let data_end = path_end
            .checked_add(size)
            .ok_or_else(|| "installer data length overflow".to_owned())?;
        if data_end > payload.len() {
            return Err("truncated installer payload".to_owned());
        }
        let path_text = std::str::from_utf8(&payload[position..path_end])
            .map_err(|_| "installer path is not UTF-8".to_owned())?;
        let path = safe_relative_path(path_text)?;
        entries.push(PayloadEntry {
            path,
            bytes: &payload[path_end..data_end],
        });
        cursor.set_position(u64::try_from(data_end).map_err(|error| error.to_string())?);
    }
    Ok(entries)
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("unsafe installer path: {value}"));
    }
    Ok(path.to_path_buf())
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    cursor.read_exact(&mut bytes).map_err(io_error)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, String> {
    let mut bytes = [0u8; 8];
    cursor.read_exact(&mut bytes).map_err(io_error)?;
    Ok(u64::from_le_bytes(bytes))
}

fn io_error(error: io::Error) -> String {
    format!("invalid installer payload: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_paths_cannot_escape_install_root() {
        assert!(safe_relative_path("panea.exe").is_ok());
        assert!(safe_relative_path("share/panea/config/default.toml").is_ok());
        assert!(safe_relative_path("../outside").is_err());
        assert!(safe_relative_path("C:/outside").is_err());
    }

    #[test]
    fn workspace_build_payload_is_well_formed() {
        assert!(parse_payload(PAYLOAD).is_ok());
    }
}
