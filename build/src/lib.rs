use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_CLI_NAME: &str = "clear-launcher";
pub const SETTINGS_FILE: &str = "settings.yml";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub launcher_path: LauncherPaths,
    pub cli_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherPaths {
    pub linux: Option<String>,
    pub macos: Option<String>,
    pub windows: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingSystem {
    Linux,
    Macos,
    Windows,
}

impl Settings {
    pub fn cli_name(&self) -> &str {
        self.cli_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(DEFAULT_CLI_NAME)
    }

    pub fn launcher_path_for(
        &self,
        os: OperatingSystem,
        env_get: &impl Fn(&str) -> Option<String>,
    ) -> Result<PathBuf> {
        let raw_path = match os {
            OperatingSystem::Linux => self
                .launcher_path
                .linux
                .as_deref()
                .unwrap_or("~/.config/clear-launcher"),
            OperatingSystem::Macos => self
                .launcher_path
                .macos
                .as_deref()
                .unwrap_or("~/Library/Application Support/clear-launcher"),
            OperatingSystem::Windows => self
                .launcher_path
                .windows
                .as_deref()
                .unwrap_or("%APPDATA%/clear-launcher"),
        };

        expand_path(raw_path, env_get)
    }
}

impl OperatingSystem {
    pub fn current() -> Result<Self> {
        match std::env::consts::OS {
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Macos),
            "windows" => Ok(Self::Windows),
            os => bail!("unsupported operating system `{os}`"),
        }
    }
}

pub fn execute(
    args: impl IntoIterator<Item = String>,
    cwd: &Path,
    env_get: &impl Fn(&str) -> Option<String>,
    stdout: &mut impl Write,
) -> Result<()> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None | Some("-h") | Some("--help") => write_usage(DEFAULT_CLI_NAME, stdout),
        Some("versions") => {
            if let Some(extra) = args.next() {
                bail!("unexpected argument for `versions`: `{extra}`");
            }

            let settings = load_settings(cwd)?;
            let launcher_root = settings.launcher_path_for(OperatingSystem::current()?, env_get)?;
            let versions = list_versions(&launcher_root)?;
            write_versions(&versions, stdout)
        }
        Some(command) => bail!("unknown command `{command}`\n\nUsage: {DEFAULT_CLI_NAME} versions"),
    }
}

pub fn load_settings(cwd: &Path) -> Result<Settings> {
    let path = find_settings_path(cwd).with_context(|| {
        format!(
            "`{SETTINGS_FILE}` was not found from `{}` or its parent directories",
            cwd.display()
        )
    })?;
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_yaml::from_str(&contents).with_context(|| format!("failed to parse `{}`", path.display()))
}

pub fn find_settings_path(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(SETTINGS_FILE))
        .find(|candidate| candidate.is_file())
}

pub fn list_versions(launcher_root: &Path) -> Result<Vec<String>> {
    let versions_dir = launcher_root.join("versions");
    let entries = match fs::read_dir(&versions_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read `{}`", versions_dir.display()));
        }
    };

    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry
            .with_context(|| format!("failed to read an entry in `{}`", versions_dir.display()))?;
        let file_type = entry.file_type().with_context(|| {
            format!(
                "failed to inspect version entry `{}`",
                entry.path().display()
            )
        })?;

        if file_type.is_dir() {
            versions.push(entry.file_name().to_string_lossy().into_owned());
        }
    }

    versions.sort_unstable();
    Ok(versions)
}

pub fn expand_path(raw_path: &str, env_get: &impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    let mut expanded = raw_path.to_owned();

    if expanded == "~" {
        expanded = env_get("HOME").context("HOME is not set, cannot expand `~`")?;
    } else if let Some(rest) = expanded.strip_prefix("~/") {
        let home = env_get("HOME").context("HOME is not set, cannot expand `~/`")?;
        expanded = format!("{home}/{rest}");
    }

    if expanded.contains("%APPDATA%") {
        let appdata =
            env_get("APPDATA").context("APPDATA is not set, cannot expand `%APPDATA%`")?;
        expanded = expanded.replace("%APPDATA%", &appdata);
    }

    Ok(PathBuf::from(expanded))
}

fn write_versions(versions: &[String], stdout: &mut impl Write) -> Result<()> {
    for version in versions {
        writeln!(stdout, "{version}").context("failed to write version output")?;
    }
    Ok(())
}

fn write_usage(cli_name: &str, stdout: &mut impl Write) -> Result<()> {
    writeln!(stdout, "Usage: {cli_name} versions").context("failed to write usage")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_env(key: &str) -> Option<String> {
        match key {
            "HOME" => Some("/home/player".to_owned()),
            "APPDATA" => Some("C:/Users/Player/AppData/Roaming".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn defaults_to_linux_launcher_path_from_recipe() {
        let settings = Settings::default();

        let path = settings
            .launcher_path_for(OperatingSystem::Linux, &test_env)
            .unwrap();

        assert_eq!(path, PathBuf::from("/home/player/.config/clear-launcher"));
    }

    #[test]
    fn parses_custom_settings() {
        let settings: Settings = serde_yaml::from_str(
            r#"
launcher_path:
  linux: "/tmp/minecraft"
cli_name: custom-launcher
"#,
        )
        .unwrap();

        assert_eq!(settings.cli_name(), "custom-launcher");
        assert_eq!(
            settings
                .launcher_path_for(OperatingSystem::Linux, &test_env)
                .unwrap(),
            PathBuf::from("/tmp/minecraft")
        );
    }

    #[test]
    fn list_versions_returns_sorted_version_directories() {
        let temp = tempfile::tempdir().unwrap();
        let versions_dir = temp.path().join("versions");
        fs::create_dir_all(versions_dir.join("1.20.4")).unwrap();
        fs::create_dir_all(versions_dir.join("1.19.4")).unwrap();
        fs::write(versions_dir.join("README.txt"), "not a version").unwrap();

        let versions = list_versions(temp.path()).unwrap();

        assert_eq!(versions, vec!["1.19.4", "1.20.4"]);
    }

    #[test]
    fn missing_versions_directory_is_empty() {
        let temp = tempfile::tempdir().unwrap();

        let versions = list_versions(temp.path()).unwrap();

        assert!(versions.is_empty());
    }

    #[test]
    fn versions_command_reads_settings_from_parent_directory() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let launcher_root = repo.path().join("launcher");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(launcher_root.join("versions").join("1.20.1")).unwrap();
        fs::create_dir_all(launcher_root.join("versions").join("23w13a_or_b")).unwrap();
        fs::write(
            repo.path().join(SETTINGS_FILE),
            format!("launcher_path:\n  linux: \"{}\"\n", launcher_root.display()),
        )
        .unwrap();

        let mut stdout = Vec::new();
        execute(vec!["versions".to_owned()], &cwd, &test_env, &mut stdout).unwrap();

        assert_eq!(String::from_utf8(stdout).unwrap(), "1.20.1\n23w13a_or_b\n");
    }
}
