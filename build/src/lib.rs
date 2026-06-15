use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_CLI_NAME: &str = "clear-launcher";
pub const SETTINGS_FILE: &str = "settings.yml";
pub const VERSION_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest.json";

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
    execute_with_version_fetcher(args, cwd, env_get, stdout, fetch_minecraft_versions)
}

fn execute_with_version_fetcher(
    args: impl IntoIterator<Item = String>,
    cwd: &Path,
    env_get: &impl Fn(&str) -> Option<String>,
    stdout: &mut impl Write,
    fetch_versions: impl FnOnce() -> Result<Vec<String>>,
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
            ensure_launcher_root(&launcher_root)?;
            let versions = fetch_versions()?;
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

pub fn ensure_launcher_root(launcher_root: &Path) -> Result<()> {
    fs::create_dir_all(launcher_root)
        .with_context(|| format!("failed to create `{}`", launcher_root.display()))
}

pub fn fetch_minecraft_versions() -> Result<Vec<String>> {
    let user_agent = format!("{DEFAULT_CLI_NAME}/{}", env!("CARGO_PKG_VERSION"));
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .user_agent(&user_agent)
        .build();

    let response = agent.get(VERSION_MANIFEST_URL).call().with_context(|| {
        format!("failed to request Minecraft version manifest from `{VERSION_MANIFEST_URL}`")
    })?;

    let manifest = response
        .into_string()
        .context("failed to read Minecraft version manifest response")?;
    parse_minecraft_versions_manifest(&manifest)
}

pub fn parse_minecraft_versions_manifest(manifest: &str) -> Result<Vec<String>> {
    let manifest: VersionManifest =
        serde_json::from_str(manifest).context("failed to parse Minecraft version manifest")?;
    Ok(manifest.into_version_ids())
}

#[derive(Debug, Deserialize)]
struct VersionManifest {
    versions: Vec<ManifestVersion>,
}

impl VersionManifest {
    fn into_version_ids(self) -> Vec<String> {
        self.versions
            .into_iter()
            .map(|version| version.id)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct ManifestVersion {
    id: String,
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
    fn parses_minecraft_versions_manifest_in_api_order() {
        let versions = parse_minecraft_versions_manifest(
            r#"
{
  "latest": {
    "release": "1.20.4",
    "snapshot": "23w13a_or_b"
  },
  "versions": [
    {
      "id": "1.20.4",
      "type": "release",
      "url": "https://example.test/1.20.4.json",
      "time": "2024-01-01T00:00:00+00:00",
      "releaseTime": "2024-01-01T00:00:00+00:00"
    },
    {
      "id": "23w13a_or_b",
      "type": "snapshot",
      "url": "https://example.test/23w13a_or_b.json",
      "time": "2023-04-01T00:00:00+00:00",
      "releaseTime": "2023-04-01T00:00:00+00:00"
    }
  ]
}
"#,
        )
        .unwrap();

        assert_eq!(versions, vec!["1.20.4", "23w13a_or_b"]);
    }

    #[test]
    fn versions_command_reads_settings_and_fetches_from_api() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let launcher_root = repo.path().join("launcher");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            repo.path().join(SETTINGS_FILE),
            format!("launcher_path:\n  linux: \"{}\"\n", launcher_root.display()),
        )
        .unwrap();

        let mut stdout = Vec::new();
        execute_with_version_fetcher(
            vec!["versions".to_owned()],
            &cwd,
            &test_env,
            &mut stdout,
            || Ok(vec!["1.20.1".to_owned(), "23w13a_or_b".to_owned()]),
        )
        .unwrap();

        assert_eq!(String::from_utf8(stdout).unwrap(), "1.20.1\n23w13a_or_b\n");
        assert!(launcher_root.is_dir());
    }
}
