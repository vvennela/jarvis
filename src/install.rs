//! Explicit installation of the observation-only Chrome native host.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub struct InstalledHost {
    pub launcher: PathBuf,
    pub manifest: PathBuf,
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Create a new registration only. Existing files are never overwritten.
#[cfg(unix)]
pub fn install_host(
    binary: &Path,
    database: &Path,
    extension_id: &str,
    host_directory: &Path,
    manifest_directory: &Path,
) -> Result<InstalledHost, Box<dyn std::error::Error>> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    crate::bridge::validate_extension_origin(
        extension_id,
        &format!("chrome-extension://{extension_id}/"),
    )?;
    for path in [binary, database, host_directory, manifest_directory] {
        if !path.is_absolute()
            || path
                .to_str()
                .is_none_or(|s| s.len() > 4096 || s.contains(['\n', '\r']))
        {
            return Err("native host paths must be absolute bounded UTF-8 paths".into());
        }
    }
    if !binary.is_file() || !database.parent().is_some_and(Path::is_dir) {
        return Err("build Jarvis and create the database parent directory first".into());
    }
    for directory in [host_directory, manifest_directory] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("native host directories must be real directories".into());
        }
    }
    let launcher = host_directory.join("jarvis-chrome-host");
    let manifest = manifest_directory.join("com.jarvis.bridge.json");
    // Reserve both paths without clobbering an existing installation.
    let mut launcher_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&launcher)?;
    let mut manifest_file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&manifest)
    {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&launcher);
            return Err(error.into());
        }
    };
    let script = format!(
        "#!/bin/sh\nexec {} native-host --db {} --extension-id {} \"$@\"\n",
        shell_word(binary.to_str().ok_or("invalid binary path")?),
        shell_word(database.to_str().ok_or("invalid database path")?),
        shell_word(extension_id),
    );
    let registration = serde_json::json!({
        "name": "com.jarvis.bridge",
        "description": "Jarvis local browser awareness (observation only)",
        "path": launcher,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")],
    });
    if let Err(error) = (|| -> Result<(), Box<dyn std::error::Error>> {
        launcher_file.write_all(script.as_bytes())?;
        launcher_file.sync_all()?;
        serde_json::to_writer_pretty(&mut manifest_file, &registration)?;
        manifest_file.write_all(b"\n")?;
        manifest_file.sync_all()?;
        Ok(())
    })() {
        let _ = fs::remove_file(&manifest);
        let _ = fs::remove_file(&launcher);
        return Err(error);
    }
    Ok(InstalledHost { launcher, manifest })
}
