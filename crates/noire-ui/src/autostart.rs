//! Explicit, user-controlled desktop-session autostart integration.

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const AUTOSTART_FILE: &str = "io.github.rayan6ms.Noire.desktop";
const MANAGED_MARKER: &str = "X-Noire-Managed=true";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Status {
    pub enabled: bool,
    pub available: bool,
}

pub(crate) fn status() -> Status {
    let Some(path) = autostart_path() else {
        return Status {
            enabled: false,
            available: false,
        };
    };
    status_at(&path, launch_command().is_some())
}

pub(crate) fn set_enabled(enabled: bool) -> io::Result<()> {
    let path = autostart_path().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no usable desktop configuration directory",
        )
    })?;
    if enabled {
        let command = launch_command().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no stable Noire launch command")
        })?;
        write_managed(&path, &command)
    } else {
        remove_managed(&path)
    }
}

fn autostart_path() -> Option<PathBuf> {
    // A sandbox-local ~/.config/autostart is not consumed by the host desktop.
    // Flatpak support must use the Background portal rather than pretending a
    // file inside the sandbox enables login startup.
    if env::var_os("FLATPAK_ID").is_some() {
        return None;
    }
    let root = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    root.is_absolute()
        .then(|| root.join("autostart").join(AUTOSTART_FILE))
}

fn launch_command() -> Option<Vec<String>> {
    if let Some(appimage) = env::var_os("APPIMAGE").filter(|value| !value.is_empty()) {
        let appimage = PathBuf::from(appimage);
        if appimage.is_absolute() {
            return Some(vec![
                appimage.to_str()?.to_owned(),
                "--minimized".to_owned(),
            ]);
        }
    }
    let executable = env::current_exe().ok()?;
    Some(vec![
        executable.to_str()?.to_owned(),
        "--minimized".to_owned(),
    ])
}

fn status_at(path: &Path, command_available: bool) -> Status {
    match fs::read_to_string(path) {
        Ok(contents) if contents.lines().any(|line| line == MANAGED_MARKER) => Status {
            enabled: true,
            available: command_available,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Status {
            enabled: false,
            available: command_available,
        },
        Ok(_) | Err(_) => Status {
            enabled: false,
            available: false,
        },
    }
}

fn write_managed(path: &Path, command: &[String]) -> io::Result<()> {
    if path.exists() {
        let contents = fs::read_to_string(path)?;
        if !contents.lines().any(|line| line == MANAGED_MARKER) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an autostart entry not owned by Noire already uses this name",
            ));
        }
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "autostart path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temporary = path.with_extension(format!("desktop.tmp-{}-{nonce}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(desktop_entry(command)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ignored = fs::remove_file(temporary);
    }
    result
}

fn remove_managed(path: &Path) -> io::Result<()> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !contents.lines().any(|line| line == MANAGED_MARKER) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the existing autostart entry is not owned by Noire",
        ));
    }
    fs::remove_file(path)
}

fn desktop_entry(command: &[String]) -> io::Result<String> {
    if command.is_empty()
        || command
            .iter()
            .any(|argument| argument.contains(['\0', '\n', '\r']))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "launch command contains an invalid argument",
        ));
    }
    let exec = command
        .iter()
        .map(|argument| {
            let escaped = argument
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('`', "\\`")
                .replace('$', "\\$")
                .replace('%', "%%");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=Noire\nComment=Local microphone noise reduction\nExec={exec}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n{MANAGED_MARKER}\n"
    ))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        env::temp_dir().join(format!(
            "noire-autostart-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn desktop_entry_quotes_appimage_paths_and_starts_minimized() -> io::Result<()> {
        let entry = desktop_entry(&[
            "/home/Test User/Noire 100%.AppImage".to_owned(),
            "--minimized".to_owned(),
        ])?;
        assert!(entry.contains("Exec=\"/home/Test User/Noire 100%%.AppImage\" \"--minimized\""));
        assert!(entry.contains(MANAGED_MARKER));
        Ok(())
    }

    #[test]
    fn managed_entries_round_trip_without_touching_foreign_files() -> io::Result<()> {
        let root = temporary_path("round-trip");
        let path = root.join(AUTOSTART_FILE);
        write_managed(&path, &["/opt/noire".to_owned(), "--minimized".to_owned()])?;
        assert_eq!(
            status_at(&path, true),
            Status {
                enabled: true,
                available: true
            }
        );
        remove_managed(&path)?;
        assert!(!path.exists());

        fs::create_dir_all(&root)?;
        fs::write(&path, "[Desktop Entry]\nName=Foreign\n")?;
        assert!(!status_at(&path, true).available);
        let removal = remove_managed(&path);
        assert_eq!(
            removal.as_ref().err().map(std::io::Error::kind),
            Some(io::ErrorKind::PermissionDenied)
        );
        assert!(path.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
