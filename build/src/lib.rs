use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_CLI_NAME: &str = "clear-launcher";
pub const SETTINGS_FILE: &str = "settings.yml";
pub const FABRIC_LOADER_VERSIONS_URL: &str = "https://meta.fabricmc.net/v2/versions/loader";
pub const MINECRAFT_VERSION_MANIFEST_URL: &str =
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
    execute_with_services(
        args,
        cwd,
        env_get,
        stdout,
        fetch_fabric_loader_versions,
        install_minecraft_version,
    )
}

fn execute_with_services(
    args: impl IntoIterator<Item = String>,
    cwd: &Path,
    env_get: &impl Fn(&str) -> Option<String>,
    stdout: &mut impl Write,
    fetch_versions: impl FnOnce() -> Result<Vec<String>>,
    install_version: impl FnOnce(&Path, &str) -> Result<InstalledVersion>,
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
        Some("install") => {
            let requested = args.next().with_context(|| {
                format!(
                    "missing version for `install`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            if let Some(extra) = args.next() {
                bail!("unexpected argument for `install`: `{extra}`");
            }

            let settings = load_settings(cwd)?;
            let launcher_root = settings.launcher_path_for(OperatingSystem::current()?, env_get)?;
            ensure_launcher_root(&launcher_root)?;
            let installed = install_version(&launcher_root, &requested)?;
            writeln!(stdout, "Installed {}", installed.id).context("failed to write install output")
        }
        Some(command) => bail!(
            "unknown command `{command}`\n\n{}",
            usage_text(DEFAULT_CLI_NAME)
        ),
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

pub fn fetch_fabric_loader_versions() -> Result<Vec<String>> {
    let user_agent = format!("{DEFAULT_CLI_NAME}/{}", env!("CARGO_PKG_VERSION"));
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .user_agent(&user_agent)
        .build();

    let response = agent
        .get(FABRIC_LOADER_VERSIONS_URL)
        .call()
        .with_context(|| {
            format!("failed to request Fabric loader versions from `{FABRIC_LOADER_VERSIONS_URL}`")
        })?;

    let versions = response
        .into_string()
        .context("failed to read Fabric loader versions response")?;
    parse_fabric_loader_versions(&versions)
}

pub fn parse_fabric_loader_versions(versions: &str) -> Result<Vec<String>> {
    let versions: Vec<FabricLoaderVersion> = serde_json::from_str(versions)
        .context("failed to parse Fabric loader versions response")?;
    Ok(versions
        .into_iter()
        .map(|version| version.version)
        .collect())
}

#[derive(Debug, Deserialize)]
struct FabricLoaderVersion {
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledVersion {
    pub id: String,
}

pub fn install_minecraft_version(
    launcher_root: &Path,
    requested_version: &str,
) -> Result<InstalledVersion> {
    let mut downloader = HttpDownloader::new();
    install_minecraft_version_with_downloader(launcher_root, requested_version, &mut downloader)
}

fn install_minecraft_version_with_downloader(
    launcher_root: &Path,
    requested_version: &str,
    downloader: &mut impl Downloader,
) -> Result<InstalledVersion> {
    let manifest = downloader.download_string(MINECRAFT_VERSION_MANIFEST_URL)?;
    let manifest = parse_minecraft_version_manifest(&manifest)?;
    let selected = resolve_minecraft_version(&manifest, requested_version)?;
    let version_id = selected.id.clone();
    let version_url = selected.url.clone();

    let version_data_json = downloader.download_string(&version_url)?;
    let version_data = parse_minecraft_version_data(&version_data_json)?;

    let version_dir = launcher_root.join("versions").join(&version_id);
    fs::create_dir_all(&version_dir)
        .with_context(|| format!("failed to create `{}`", version_dir.display()))?;
    fs::write(
        version_dir.join(format!("{version_id}.json")),
        &version_data_json,
    )
    .with_context(|| format!("failed to write version data for `{version_id}`"))?;

    let client = version_data
        .downloads
        .client
        .context("selected Minecraft version does not include a client download")?;
    downloader.download_to_path(&client.url, &version_dir.join(format!("{version_id}.jar")))?;

    if let Some(asset_index) = version_data.asset_index {
        install_assets(launcher_root, &asset_index, downloader)?;
    }

    install_libraries(launcher_root, version_data.libraries, downloader)?;

    Ok(InstalledVersion { id: version_id })
}

fn install_assets(
    launcher_root: &Path,
    asset_index: &AssetIndex,
    downloader: &mut impl Downloader,
) -> Result<()> {
    let asset_index_json = downloader.download_string(&asset_index.url)?;
    let indexes_dir = launcher_root.join("assets").join("indexes");
    fs::create_dir_all(&indexes_dir)
        .with_context(|| format!("failed to create `{}`", indexes_dir.display()))?;
    fs::write(
        indexes_dir.join(format!("{}.json", asset_index.id)),
        &asset_index_json,
    )
    .with_context(|| format!("failed to write asset index `{}`", asset_index.id))?;

    let asset_objects = parse_asset_objects(&asset_index_json)?;
    for object in asset_objects.objects.values() {
        let prefix = object
            .hash
            .get(..2)
            .with_context(|| format!("asset hash `{}` is too short", object.hash))?;
        let object_url = format!(
            "https://resources.download.minecraft.net/{}/{}",
            prefix, object.hash
        );
        let object_path = launcher_root
            .join("assets")
            .join("objects")
            .join(prefix)
            .join(&object.hash);
        downloader.download_to_path(&object_url, &object_path)?;
    }

    Ok(())
}

fn install_libraries(
    launcher_root: &Path,
    libraries: Vec<MinecraftLibrary>,
    downloader: &mut impl Downloader,
) -> Result<()> {
    for library in libraries {
        let Some(artifact) = library.downloads.and_then(|downloads| downloads.artifact) else {
            continue;
        };
        let Some(path) = artifact.path else {
            continue;
        };
        downloader.download_to_path(&artifact.url, &launcher_root.join("libraries").join(path))?;
    }

    Ok(())
}

fn parse_minecraft_version_manifest(manifest: &str) -> Result<MinecraftVersionManifest> {
    serde_json::from_str(manifest).context("failed to parse Minecraft version manifest")
}

fn parse_minecraft_version_data(version_data: &str) -> Result<MinecraftVersionData> {
    serde_json::from_str(version_data).context("failed to parse Minecraft version data")
}

fn parse_asset_objects(asset_index: &str) -> Result<AssetObjects> {
    serde_json::from_str(asset_index).context("failed to parse Minecraft asset index")
}

fn resolve_minecraft_version<'a>(
    manifest: &'a MinecraftVersionManifest,
    requested_version: &str,
) -> Result<&'a MinecraftManifestVersion> {
    let requested_version = requested_version.trim();
    if requested_version.is_empty() {
        bail!("version cannot be empty");
    }

    if requested_version == "latest" {
        return manifest
            .versions
            .iter()
            .find(|version| version.id == manifest.latest.release)
            .with_context(|| {
                format!(
                    "latest release `{}` was not found in the Minecraft version manifest",
                    manifest.latest.release
                )
            });
    }

    if is_major_version_request(requested_version) {
        let prefix = format!("{requested_version}.");
        return manifest
            .versions
            .iter()
            .find(|version| {
                version.version_type == "release"
                    && (version.id == requested_version || version.id.starts_with(&prefix))
            })
            .with_context(|| {
                format!("no Minecraft release found for major version `{requested_version}`")
            });
    }

    manifest
        .versions
        .iter()
        .find(|version| version.id == requested_version)
        .with_context(|| format!("Minecraft version `{requested_version}` was not found"))
}

fn is_major_version_request(version: &str) -> bool {
    let mut parts = version.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !major.is_empty()
        && !minor.is_empty()
        && major.chars().all(|character| character.is_ascii_digit())
        && minor.chars().all(|character| character.is_ascii_digit())
}

trait Downloader {
    fn download_string(&mut self, url: &str) -> Result<String>;
    fn download_to_path(&mut self, url: &str, path: &Path) -> Result<()>;
}

struct HttpDownloader {
    agent: ureq::Agent,
}

impl HttpDownloader {
    fn new() -> Self {
        let user_agent = format!("{DEFAULT_CLI_NAME}/{}", env!("CARGO_PKG_VERSION"));
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .user_agent(&user_agent)
            .build();

        Self { agent }
    }
}

impl Downloader for HttpDownloader {
    fn download_string(&mut self, url: &str) -> Result<String> {
        self.agent
            .get(url)
            .call()
            .with_context(|| format!("failed to request `{url}`"))?
            .into_string()
            .with_context(|| format!("failed to read response from `{url}`"))
    }

    fn download_to_path(&mut self, url: &str, path: &Path) -> Result<()> {
        if path.is_file() {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }

        let response = self
            .agent
            .get(url)
            .call()
            .with_context(|| format!("failed to download `{url}`"))?;
        let mut reader = response.into_reader();
        let mut file = fs::File::create(path)
            .with_context(|| format!("failed to create `{}`", path.display()))?;
        io::copy(&mut reader, &mut file)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct MinecraftVersionManifest {
    latest: LatestMinecraftVersions,
    versions: Vec<MinecraftManifestVersion>,
}

#[derive(Debug, Deserialize)]
struct LatestMinecraftVersions {
    release: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MinecraftManifestVersion {
    id: String,
    #[serde(rename = "type")]
    version_type: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct MinecraftVersionData {
    #[serde(rename = "assetIndex")]
    asset_index: Option<AssetIndex>,
    downloads: MinecraftDownloads,
    #[serde(default)]
    libraries: Vec<MinecraftLibrary>,
}

#[derive(Debug, Deserialize)]
struct MinecraftDownloads {
    client: Option<DownloadInfo>,
}

#[derive(Debug, Deserialize)]
struct MinecraftLibrary {
    downloads: Option<LibraryDownloads>,
}

#[derive(Debug, Deserialize)]
struct LibraryDownloads {
    artifact: Option<DownloadInfo>,
}

#[derive(Debug, Deserialize)]
struct DownloadInfo {
    path: Option<String>,
    url: String,
}

#[derive(Debug, Deserialize)]
struct AssetIndex {
    id: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct AssetObjects {
    objects: HashMap<String, AssetObject>,
}

#[derive(Debug, Deserialize)]
struct AssetObject {
    hash: String,
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
    write!(stdout, "{}", usage_text(cli_name)).context("failed to write usage")
}

fn usage_text(cli_name: &str) -> String {
    format!("Usage: {cli_name} versions\n       {cli_name} install {{version}}|latest\n")
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
    fn parses_fabric_loader_versions_in_api_order() {
        let versions = parse_fabric_loader_versions(
            r#"
[
  {
    "separator": ".",
    "build": 3,
    "maven": "net.fabricmc:fabric-loader:0.19.3",
    "version": "0.19.3",
    "stable": true
  },
  {
    "separator": "+build.",
    "build": 214,
    "maven": "net.fabricmc:fabric-loader:0.10.6+build.214",
    "version": "0.10.6+build.214",
    "stable": false
  }
]
"#,
        )
        .unwrap();

        assert_eq!(versions, vec!["0.19.3", "0.10.6+build.214"]);
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
        execute_with_services(
            vec!["versions".to_owned()],
            &cwd,
            &test_env,
            &mut stdout,
            || Ok(vec!["0.19.3".to_owned(), "0.10.6+build.214".to_owned()]),
            |_, _| unreachable!("versions command should not install versions"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "0.19.3\n0.10.6+build.214\n"
        );
        assert!(launcher_root.is_dir());
    }

    #[test]
    fn resolves_latest_major_and_exact_minecraft_versions() {
        let manifest = parse_minecraft_version_manifest(
            r#"
{
  "latest": {
    "release": "1.20.4",
    "snapshot": "23w13a_or_b"
  },
  "versions": [
    {
      "id": "23w13a_or_b",
      "type": "snapshot",
      "url": "https://example.test/23w13a_or_b.json"
    },
    {
      "id": "1.20.4",
      "type": "release",
      "url": "https://example.test/1.20.4.json"
    },
    {
      "id": "1.18.2",
      "type": "release",
      "url": "https://example.test/1.18.2.json"
    },
    {
      "id": "1.18.1",
      "type": "release",
      "url": "https://example.test/1.18.1.json"
    },
    {
      "id": "1.18",
      "type": "release",
      "url": "https://example.test/1.18.json"
    }
  ]
}
"#,
        )
        .unwrap();

        assert_eq!(
            resolve_minecraft_version(&manifest, "latest").unwrap().id,
            "1.20.4"
        );
        assert_eq!(
            resolve_minecraft_version(&manifest, "1.18").unwrap().id,
            "1.18.2"
        );
        assert_eq!(
            resolve_minecraft_version(&manifest, "23w13a_or_b")
                .unwrap()
                .id,
            "23w13a_or_b"
        );
    }

    #[test]
    fn install_command_reads_settings_and_installs_requested_version() {
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
        execute_with_services(
            vec!["install".to_owned(), "1.18".to_owned()],
            &cwd,
            &test_env,
            &mut stdout,
            || unreachable!("install command should not fetch Fabric versions"),
            |root, requested| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(requested, "1.18");
                assert!(root.is_dir());
                Ok(InstalledVersion {
                    id: "1.18.2".to_owned(),
                })
            },
        )
        .unwrap();

        assert_eq!(String::from_utf8(stdout).unwrap(), "Installed 1.18.2\n");
    }

    #[test]
    fn installs_minecraft_version_files_from_manifest() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let asset_hash = "abcdef0123456789abcdef0123456789abcdef01";
        let version_data = r#"
{
  "assetIndex": {
    "id": "1.18",
    "url": "https://example.test/assets/1.18.json"
  },
  "downloads": {
    "client": {
      "url": "https://example.test/client.jar"
    }
  },
  "libraries": [
    {
      "downloads": {
        "artifact": {
          "path": "org/example/lib/1.0/lib-1.0.jar",
          "url": "https://example.test/lib-1.0.jar"
        }
      }
    }
  ]
}
"#;

        let mut downloader = FakeDownloader::new([
            (
                MINECRAFT_VERSION_MANIFEST_URL,
                r#"
{
  "latest": {
    "release": "1.18.2",
    "snapshot": "22w16b"
  },
  "versions": [
    {
      "id": "1.18.2",
      "type": "release",
      "url": "https://example.test/1.18.2.json"
    }
  ]
}
"#,
            ),
            ("https://example.test/1.18.2.json", version_data),
            (
                "https://example.test/assets/1.18.json",
                &format!(
                    r#"{{
  "objects": {{
    "icons/icon_16x16.png": {{
      "hash": "{asset_hash}"
    }}
  }}
}}"#
                ),
            ),
        ]);

        let installed =
            install_minecraft_version_with_downloader(&launcher_root, "latest", &mut downloader)
                .unwrap();

        assert_eq!(installed.id, "1.18.2");
        assert_eq!(
            fs::read_to_string(launcher_root.join("versions/1.18.2/1.18.2.json")).unwrap(),
            version_data
        );
        assert_eq!(
            fs::read(launcher_root.join("versions/1.18.2/1.18.2.jar")).unwrap(),
            b"https://example.test/client.jar"
        );
        assert!(launcher_root.join("assets/indexes/1.18.json").is_file());
        assert_eq!(
            fs::read(
                launcher_root
                    .join("assets")
                    .join("objects")
                    .join("ab")
                    .join(asset_hash)
            )
            .unwrap(),
            format!("https://resources.download.minecraft.net/ab/{asset_hash}").as_bytes()
        );
        assert_eq!(
            fs::read(launcher_root.join("libraries/org/example/lib/1.0/lib-1.0.jar")).unwrap(),
            b"https://example.test/lib-1.0.jar"
        );
    }

    struct FakeDownloader {
        strings: HashMap<String, String>,
    }

    impl FakeDownloader {
        fn new<const N: usize>(strings: [(&str, &str); N]) -> Self {
            Self {
                strings: strings
                    .into_iter()
                    .map(|(url, response)| (url.to_owned(), response.to_owned()))
                    .collect(),
            }
        }
    }

    impl Downloader for FakeDownloader {
        fn download_string(&mut self, url: &str) -> Result<String> {
            self.strings
                .get(url)
                .cloned()
                .with_context(|| format!("missing fake string response for `{url}`"))
        }

        fn download_to_path(&mut self, url: &str, path: &Path) -> Result<()> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create `{}`", parent.display()))?;
            }
            fs::write(path, url).with_context(|| format!("failed to write `{}`", path.display()))
        }
    }
}
