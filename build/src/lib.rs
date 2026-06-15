use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const DEFAULT_CLI_NAME: &str = env!("CLEAR_LAUNCHER_CLI_NAME");
pub const DEFAULT_LINUX_LAUNCHER_PATH: &str = env!("CLEAR_LAUNCHER_LAUNCHER_PATH_LINUX");
pub const DEFAULT_MACOS_LAUNCHER_PATH: &str = env!("CLEAR_LAUNCHER_LAUNCHER_PATH_MACOS");
pub const DEFAULT_WINDOWS_LAUNCHER_PATH: &str = env!("CLEAR_LAUNCHER_LAUNCHER_PATH_WINDOWS");
pub const CONFIG_FILE_NAME: &str = "config.yml";
pub const BUILD_FOLDER_NAME: &str = "build";
pub const VERSIONS_FOLDER_NAME: &str = "versions";
pub const MODS_FOLDER_NAME: &str = "mods";
pub const DEFAULT_INSTALL_ALIAS: &str = "default";
pub const FABRIC_LOADER_VERSIONS_URL: &str = "https://meta.fabricmc.net/v2/versions/loader";
pub const FABRIC_GAME_VERSIONS_URL: &str = "https://meta.fabricmc.net/v2/versions/game";
pub const MINECRAFT_VERSION_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledSettings {
    pub launcher_path: CompiledLauncherPaths,
    pub cli_name: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledLauncherPaths {
    pub linux: &'static str,
    pub macos: &'static str,
    pub windows: &'static str,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherConfig {
    pub path_symlink: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingSystem {
    Linux,
    Macos,
    Windows,
}

pub const COMPILED_SETTINGS: CompiledSettings = CompiledSettings {
    launcher_path: CompiledLauncherPaths {
        linux: DEFAULT_LINUX_LAUNCHER_PATH,
        macos: DEFAULT_MACOS_LAUNCHER_PATH,
        windows: DEFAULT_WINDOWS_LAUNCHER_PATH,
    },
    cli_name: DEFAULT_CLI_NAME,
};

impl CompiledSettings {
    pub fn cli_name(&self) -> &str {
        self.cli_name
    }

    pub fn launcher_path_for(
        &self,
        os: OperatingSystem,
        env_get: &impl Fn(&str) -> Option<String>,
    ) -> Result<PathBuf> {
        let raw_path = match os {
            OperatingSystem::Linux => self.launcher_path.linux,
            OperatingSystem::Macos => self.launcher_path.macos,
            OperatingSystem::Windows => self.launcher_path.windows,
        };

        expand_path(raw_path, env_get)
    }
}

pub fn compiled_settings() -> CompiledSettings {
    COMPILED_SETTINGS
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
    stderr: &mut impl Write,
) -> Result<()> {
    execute_with_services(
        args,
        cwd,
        env_get,
        stdout,
        stderr,
        fetch_fabric_minecraft_versions,
        |launcher_root, versions_folder, request| {
            install_minecraft_version_with_alias(
                launcher_root,
                versions_folder,
                &request.requested_version,
                request.alias.as_deref(),
            )
        },
        |launcher_root, versions_folder, request| {
            run_minecraft_version_offline(launcher_root, versions_folder, request)
        },
        current_exe_path,
    )
}

fn execute_with_services(
    args: impl IntoIterator<Item = String>,
    cwd: &Path,
    env_get: &impl Fn(&str) -> Option<String>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    fetch_versions: impl FnOnce() -> Result<Vec<FabricMinecraftVersion>>,
    install_version: impl FnOnce(&Path, &Path, &InstallRequest) -> Result<InstalledVersion>,
    run_version: impl FnOnce(&Path, &Path, &RunRequest) -> Result<LaunchedVersion>,
    current_exe: impl FnOnce() -> Result<PathBuf>,
) -> Result<()> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None | Some("-h") | Some("--help") => write_usage(DEFAULT_CLI_NAME, stdout),
        Some("versions") => {
            if let Some(extra) = args.next() {
                bail!("unexpected argument for `versions`: `{extra}`");
            }

            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            write_log(
                stderr,
                format_args!("Fetching Fabric-supported Minecraft versions"),
            )?;
            let versions = fetch_versions()?;
            write_log(
                stderr,
                format_args!(
                    "Writing {} Fabric-supported Minecraft versions",
                    versions.len()
                ),
            )?;
            write_versions(&versions, stdout)?;
            write_log(stderr, format_args!("Finished listing versions"))
        }
        Some("install") => {
            let requested_version = args.next().with_context(|| {
                format!(
                    "missing version for `install`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            let install_request = parse_install_request(requested_version, args)?;

            write_log(
                stderr,
                format_args!(
                    "Preparing install for requested Minecraft version `{}`",
                    install_request.requested_version
                ),
            )?;
            if let Some(alias) = install_request.alias.as_deref() {
                write_log(stderr, format_args!("Using install alias `{alias}`"))?;
            }
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
            write_log(
                stderr,
                format_args!("Using versions folder {}", versions_folder.display()),
            )?;
            let installed = install_version(&launcher_root, &versions_folder, &install_request)?;
            write_log(
                stderr,
                format_args!("Writing install result for `{}`", installed.id),
            )?;
            write_install_output(&installed, stdout)?;
            write_log(
                stderr,
                format_args!("Finished installing `{}`", installed.id),
            )
        }
        Some("run") => {
            let requested_version = args.next().with_context(|| {
                format!(
                    "missing version for `run`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            let run_request = parse_run_request(requested_version, args)?;

            write_log(
                stderr,
                format_args!(
                    "Preparing run for requested Minecraft version `{}`",
                    run_request.requested_version
                ),
            )?;
            if let Some(alias) = run_request.alias.as_deref() {
                write_log(stderr, format_args!("Using run alias `{alias}`"))?;
            }
            write_log(
                stderr,
                format_args!("Using offline username `{}`", run_request.username),
            )?;
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
            write_log(
                stderr,
                format_args!("Using versions folder {}", versions_folder.display()),
            )?;
            let launched = run_version(&launcher_root, &versions_folder, &run_request)?;
            write_run_output(&launched, stdout)?;
            write_log(
                stderr,
                format_args!(
                    "Finished running `{}` as `{}`",
                    launched.id, launched.username
                ),
            )
        }
        Some("configure-path") => {
            if let Some(extra) = args.next() {
                bail!("unexpected argument for `configure-path`: `{extra}`");
            }

            write_log(stderr, format_args!("Configuring CLI path"))?;
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            let cli_name = COMPILED_SETTINGS.cli_name();
            write_log(stderr, format_args!("Installing `{cli_name}` into PATH"))?;
            let symlink_path =
                configure_path(&launcher_root, cli_name, &current_exe()?, cwd, env_get)?;
            writeln!(
                stdout,
                "Configured {cli_name} at {}",
                symlink_path.display()
            )
            .context("failed to write configure-path output")?;
            write_log(
                stderr,
                format_args!("Configured CLI path at {}", symlink_path.display()),
            )
        }
        Some("unset-path") => {
            if let Some(extra) = args.next() {
                bail!("unexpected argument for `unset-path`: `{extra}`");
            }

            write_log(stderr, format_args!("Unsetting CLI path"))?;
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Reading path config from {}", launcher_root.display()),
            )?;
            let unset = unset_path(&launcher_root)?;
            match unset.as_ref() {
                Some(path) => writeln!(stdout, "Unset {}", path.display()),
                None => writeln!(stdout, "No path symlink configured"),
            }
            .context("failed to write unset-path output")?;
            match unset {
                Some(path) => write_log(
                    stderr,
                    format_args!("Removed configured CLI path {}", path.display()),
                ),
                None => write_log(stderr, format_args!("No configured CLI path to remove")),
            }
        }
        Some(command) => bail!(
            "unknown command `{command}`\n\n{}",
            usage_text(DEFAULT_CLI_NAME)
        ),
    }
}

fn write_log(stderr: &mut impl Write, message: fmt::Arguments<'_>) -> Result<()> {
    writeln!(stderr, "{DEFAULT_CLI_NAME}: {message}").context("failed to write CLI log")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRequest {
    pub requested_version: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub requested_version: String,
    pub alias: Option<String>,
    pub username: String,
}

fn parse_install_request(
    requested_version: String,
    mut args: impl Iterator<Item = String>,
) -> Result<InstallRequest> {
    let mut alias = None;

    while let Some(arg) = args.next() {
        if arg == "--alias" {
            let value = args
                .next()
                .context("missing value for `--alias` in `install`")?;
            set_install_alias(&mut alias, value)?;
        } else if let Some(value) = arg.strip_prefix("--alias=") {
            set_install_alias(&mut alias, value.to_owned())?;
        } else {
            bail!("unexpected argument for `install`: `{arg}`");
        }
    }

    Ok(InstallRequest {
        requested_version,
        alias,
    })
}

fn parse_run_request(
    requested_version: String,
    mut args: impl Iterator<Item = String>,
) -> Result<RunRequest> {
    let mut alias = None;
    let mut username = None;

    while let Some(arg) = args.next() {
        if arg == "--alias" {
            let value = args
                .next()
                .context("missing value for `--alias` in `run`")?;
            set_run_alias(&mut alias, value)?;
        } else if let Some(value) = arg.strip_prefix("--alias=") {
            set_run_alias(&mut alias, value.to_owned())?;
        } else if arg == "--username" {
            let value = args
                .next()
                .context("missing value for `--username` in `run`")?;
            set_run_username(&mut username, value)?;
        } else if let Some(value) = arg.strip_prefix("--username=") {
            set_run_username(&mut username, value.to_owned())?;
        } else {
            bail!("unexpected argument for `run`: `{arg}`");
        }
    }

    let username = username.context("missing required `--username` for `run`")?;

    Ok(RunRequest {
        requested_version,
        alias,
        username,
    })
}

fn set_install_alias(alias: &mut Option<String>, value: String) -> Result<()> {
    set_alias(alias, value, "install alias")
}

fn set_run_alias(alias: &mut Option<String>, value: String) -> Result<()> {
    set_alias(alias, value, "run alias")
}

fn set_alias(alias: &mut Option<String>, value: String, label: &str) -> Result<()> {
    if alias.is_some() {
        bail!("`--alias` was provided more than once");
    }
    validate_path_segment(&value, label)?;
    *alias = Some(value);
    Ok(())
}

fn set_run_username(username: &mut Option<String>, value: String) -> Result<()> {
    if username.is_some() {
        bail!("`--username` was provided more than once");
    }
    validate_username(&value)?;
    *username = Some(value);
    Ok(())
}

fn launcher_root_from_compiled_settings(
    env_get: &impl Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    COMPILED_SETTINGS.launcher_path_for(OperatingSystem::current()?, env_get)
}

pub fn ensure_launcher_root(launcher_root: &Path) -> Result<()> {
    fs::create_dir_all(launcher_root)
        .with_context(|| format!("failed to create `{}`", launcher_root.display()))
}

pub fn load_launcher_config(config_file: &Path) -> Result<LauncherConfig> {
    if !config_file.is_file() {
        return Ok(LauncherConfig::default());
    }

    let contents = fs::read_to_string(config_file)
        .with_context(|| format!("failed to read `{}`", config_file.display()))?;
    if contents.trim().is_empty() {
        return Ok(LauncherConfig::default());
    }

    serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse `{}`", config_file.display()))
}

pub fn save_launcher_config(config_file: &Path, config: &LauncherConfig) -> Result<()> {
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let contents = serde_yaml::to_string(config).context("failed to serialize launcher config")?;
    fs::write(config_file, contents)
        .with_context(|| format!("failed to write `{}`", config_file.display()))
}

fn launcher_config_file(launcher_root: &Path) -> PathBuf {
    launcher_root.join(CONFIG_FILE_NAME)
}

pub fn configure_path(
    launcher_root: &Path,
    cli_name: &str,
    current_exe: &Path,
    cwd: &Path,
    env_get: &impl Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    ensure_launcher_root(launcher_root)?;
    validate_path_segment(cli_name, "CLI name")?;

    let symlink_path = install_cli_symlink(cli_name, current_exe, cwd, env_get)?;
    let config_file = launcher_config_file(launcher_root);
    let mut config = load_launcher_config(&config_file)?;
    config.path_symlink = Some(symlink_path.clone());
    save_launcher_config(&config_file, &config)?;

    Ok(symlink_path)
}

pub fn unset_path(launcher_root: &Path) -> Result<Option<PathBuf>> {
    ensure_launcher_root(launcher_root)?;

    let config_file = launcher_config_file(launcher_root);
    let mut config = load_launcher_config(&config_file)?;
    let Some(symlink_path) = config.path_symlink.take() else {
        save_launcher_config(&config_file, &config)?;
        return Ok(None);
    };

    match fs::symlink_metadata(&symlink_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(&symlink_path)
                .with_context(|| format!("failed to remove `{}`", symlink_path.display()))?;
        }
        Ok(_) => {
            bail!(
                "configured path `{}` is not a symlink; refusing to remove it",
                symlink_path.display()
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect `{}`", symlink_path.display()));
        }
    }

    save_launcher_config(&config_file, &config)?;
    Ok(Some(symlink_path))
}

fn install_cli_symlink(
    cli_name: &str,
    current_exe: &Path,
    cwd: &Path,
    env_get: &impl Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    let path_var = env_get("PATH").context("PATH is not set, cannot configure CLI path")?;
    let mut failures = Vec::new();

    for path_dir in std::env::split_paths(&path_var) {
        let path_dir = absolute_path_from_entry(&path_dir, cwd);
        if !path_dir.is_dir() {
            continue;
        }

        let symlink_path = path_dir.join(cli_name);
        match fs::symlink_metadata(&symlink_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let linked_to = fs::read_link(&symlink_path)
                    .with_context(|| format!("failed to read `{}`", symlink_path.display()))?;
                let linked_to = absolute_link_target(&symlink_path, &linked_to);
                if same_files(&linked_to, current_exe) {
                    return Ok(symlink_path);
                }

                failures.push(format!(
                    "`{}` already links to `{}`",
                    symlink_path.display(),
                    linked_to.display()
                ));
                continue;
            }
            Ok(_) => {
                failures.push(format!(
                    "`{}` already exists and is not a symlink",
                    symlink_path.display()
                ));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                failures.push(format!(
                    "failed to inspect `{}`: {error}",
                    symlink_path.display()
                ));
                continue;
            }
        }

        match create_cli_symlink(current_exe, &symlink_path) {
            Ok(()) => return Ok(symlink_path),
            Err(error) => failures.push(format!(
                "failed to create `{}`: {error}",
                symlink_path.display()
            )),
        }
    }

    if failures.is_empty() {
        bail!("could not find an existing PATH directory for `{cli_name}`");
    }

    bail!(
        "could not create `{cli_name}` in any PATH directory: {}",
        failures.join("; ")
    )
}

fn absolute_path_from_entry(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn absolute_link_target(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(target)
    }
}

fn same_files(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

#[cfg(unix)]
fn create_cli_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_cli_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn current_exe_path() -> Result<PathBuf> {
    std::env::current_exe().context("failed to determine current executable path")
}

pub fn fetch_fabric_minecraft_versions() -> Result<Vec<FabricMinecraftVersion>> {
    let mut downloader = HttpDownloader::new();
    fetch_fabric_minecraft_versions_with_downloader(&mut downloader)
}

pub fn fetch_fabric_loader_versions() -> Result<Vec<String>> {
    let mut downloader = HttpDownloader::new();
    let versions = downloader.download_string(FABRIC_LOADER_VERSIONS_URL)?;
    parse_fabric_loader_versions(&versions)
}

fn fetch_fabric_minecraft_versions_with_downloader(
    downloader: &mut impl Downloader,
) -> Result<Vec<FabricMinecraftVersion>> {
    let game_versions = downloader.download_string(FABRIC_GAME_VERSIONS_URL)?;
    let game_versions = parse_fabric_game_versions(&game_versions)?;
    let loader_versions = downloader.download_string(FABRIC_LOADER_VERSIONS_URL)?;
    let loader_versions = parse_fabric_loader_version_metadata(&loader_versions)?;
    let fabric_version = select_fabric_loader_version(&loader_versions)
        .context("Fabric loader versions response did not include any loader versions")?
        .to_owned();

    Ok(game_versions
        .into_iter()
        .map(|minecraft_version| FabricMinecraftVersion {
            minecraft_version,
            fabric_version: fabric_version.clone(),
        })
        .collect())
}

pub fn parse_fabric_game_versions(versions: &str) -> Result<Vec<String>> {
    let versions: Vec<FabricGameVersion> =
        serde_json::from_str(versions).context("failed to parse Fabric game versions response")?;
    Ok(versions
        .into_iter()
        .map(|version| version.version)
        .collect())
}

pub fn parse_fabric_loader_versions(versions: &str) -> Result<Vec<String>> {
    Ok(parse_fabric_loader_version_metadata(versions)?
        .into_iter()
        .map(|version| version.version)
        .collect())
}

fn parse_fabric_loader_version_metadata(versions: &str) -> Result<Vec<FabricLoaderVersion>> {
    serde_json::from_str(versions).context("failed to parse Fabric loader versions response")
}

#[derive(Debug, Deserialize)]
struct FabricGameVersion {
    version: String,
}

#[derive(Debug, Clone, Deserialize)]
struct FabricLoaderVersion {
    version: String,
    #[serde(default)]
    stable: bool,
}

#[derive(Debug, Deserialize)]
struct FabricMinecraftLoaderVersion {
    loader: FabricLoaderVersion,
}

fn parse_fabric_loader_versions_for_minecraft_version(
    versions: &str,
) -> Result<Vec<FabricLoaderVersion>> {
    let versions: Vec<FabricMinecraftLoaderVersion> = serde_json::from_str(versions)
        .context("failed to parse Fabric loader versions for Minecraft version response")?;
    Ok(versions.into_iter().map(|version| version.loader).collect())
}

fn select_fabric_loader_version(versions: &[FabricLoaderVersion]) -> Option<&str> {
    versions
        .iter()
        .find(|version| version.stable)
        .or_else(|| versions.first())
        .map(|version| version.version.as_str())
}

fn resolve_fabric_loader_version(
    downloader: &mut impl Downloader,
    minecraft_version: &str,
) -> Result<String> {
    let versions_url = fabric_loader_versions_for_minecraft_url(minecraft_version);
    let versions = downloader.download_string(&versions_url)?;
    let versions = parse_fabric_loader_versions_for_minecraft_version(&versions)?;
    select_fabric_loader_version(&versions)
        .with_context(|| {
            format!("no Fabric loader version found for Minecraft `{minecraft_version}`")
        })
        .map(str::to_owned)
}

fn fabric_loader_versions_for_minecraft_url(minecraft_version: &str) -> String {
    format!("{FABRIC_LOADER_VERSIONS_URL}/{minecraft_version}")
}

fn fabric_profile_url(minecraft_version: &str, loader_version: &str) -> String {
    format!("{FABRIC_LOADER_VERSIONS_URL}/{minecraft_version}/{loader_version}/profile/json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricMinecraftVersion {
    pub minecraft_version: String,
    pub fabric_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledVersion {
    pub id: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchedVersion {
    pub id: String,
    pub alias: Option<String>,
    pub username: String,
}

pub fn run_minecraft_version_offline(
    launcher_root: &Path,
    versions_folder: &Path,
    request: &RunRequest,
) -> Result<LaunchedVersion> {
    let mut downloader = HttpDownloader::new();
    let mut launcher = ProcessJavaLauncher;
    run_minecraft_version_offline_with_services(
        launcher_root,
        versions_folder,
        request,
        &mut downloader,
        &mut launcher,
    )
}

fn run_minecraft_version_offline_with_services(
    launcher_root: &Path,
    versions_folder: &Path,
    request: &RunRequest,
    downloader: &mut impl Downloader,
    launcher: &mut impl JavaLauncher,
) -> Result<LaunchedVersion> {
    validate_username(&request.username)?;
    if let Some(alias) = request.alias.as_deref() {
        validate_path_segment(alias, "run alias")?;
    }

    let manifest = downloader.download_string(MINECRAFT_VERSION_MANIFEST_URL)?;
    let manifest = parse_minecraft_version_manifest(&manifest)?;
    let selected = resolve_minecraft_version(&manifest, &request.requested_version)?;
    let version_id = selected.id.clone();
    validate_path_segment(&version_id, "resolved version id")?;
    let install_name = request.alias.as_deref().unwrap_or(DEFAULT_INSTALL_ALIAS);
    validate_path_segment(install_name, "run alias")?;

    let version_dir = versions_folder.join(&version_id).join(install_name);
    if !version_dir
        .try_exists()
        .with_context(|| format!("failed to inspect `{}`", version_dir.display()))?
    {
        bail!(
            "Minecraft version `{version_id}` with alias `{install_name}` is not installed at `{}`",
            version_dir.display()
        );
    }
    ensure_version_mods_dir(&version_dir)?;

    let command = build_launch_command(
        launcher_root,
        &version_dir,
        &version_id,
        install_name,
        request,
    )?;
    launcher.launch(&command)?;

    Ok(LaunchedVersion {
        id: version_id,
        alias: request.alias.clone(),
        username: request.username.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JavaLaunchCommand {
    program: String,
    args: Vec<String>,
    current_dir: PathBuf,
}

trait JavaLauncher {
    fn launch(&mut self, command: &JavaLaunchCommand) -> Result<()>;
}

struct ProcessJavaLauncher;

impl JavaLauncher for ProcessJavaLauncher {
    fn launch(&mut self, command: &JavaLaunchCommand) -> Result<()> {
        let status = Command::new(&command.program)
            .args(&command.args)
            .current_dir(&command.current_dir)
            .status()
            .with_context(|| format!("failed to start `{}`", command.program))?;

        if !status.success() {
            match status.code() {
                Some(code) => bail!("Minecraft exited with status code {code}"),
                None => bail!("Minecraft was terminated by signal"),
            }
        }

        Ok(())
    }
}

fn build_launch_command(
    launcher_root: &Path,
    version_dir: &Path,
    version_id: &str,
    install_name: &str,
    request: &RunRequest,
) -> Result<JavaLaunchCommand> {
    let version_json_path = version_dir.join(format!("{install_name}.json"));
    let version_json = fs::read_to_string(&version_json_path)
        .with_context(|| format!("failed to read `{}`", version_json_path.display()))?;
    let version_data = parse_minecraft_version_data(&version_json)?;
    let main_class = version_data
        .main_class
        .as_deref()
        .context("installed Minecraft version does not include a main class")?;
    let client_jar = version_dir.join(format!("{install_name}.jar"));
    if !client_jar.is_file() {
        bail!(
            "installed Minecraft client jar is missing at `{}`",
            client_jar.display()
        );
    }

    let classpath = build_classpath(launcher_root, &client_jar, &version_data.libraries)?;
    let context = LaunchArgumentContext {
        launcher_root,
        version_dir,
        version_id,
        username: &request.username,
        classpath: &classpath,
        client_jar: &client_jar,
        asset_index_name: version_data
            .asset_index
            .as_ref()
            .map(|asset_index| asset_index.id.as_str())
            .unwrap_or(version_id),
        version_type: version_data.version_type.as_deref().unwrap_or("release"),
        offline_uuid: offline_player_uuid(&request.username),
    };

    let jvm_templates = version_data
        .arguments
        .as_ref()
        .map(|arguments| collect_launch_arguments(&arguments.jvm))
        .transpose()?
        .unwrap_or_default();
    let mut jvm_args = jvm_templates
        .iter()
        .map(|argument| substitute_launch_argument(argument, &context))
        .collect::<Vec<_>>();
    if !launch_arguments_supply_classpath(&jvm_templates) {
        jvm_args.push("-Djava.library.path=${natives_directory}".to_owned());
        jvm_args.push("-cp".to_owned());
        jvm_args.push("${classpath}".to_owned());
        jvm_args = jvm_args
            .iter()
            .map(|argument| substitute_launch_argument(argument, &context))
            .collect();
    }

    let game_templates = game_launch_arguments(&version_data)?;
    let game_args = game_templates
        .iter()
        .map(|argument| substitute_launch_argument(argument, &context))
        .collect::<Vec<_>>();

    let mut args = Vec::with_capacity(jvm_args.len() + 1 + game_args.len());
    args.extend(jvm_args);
    args.push(main_class.to_owned());
    args.extend(game_args);

    Ok(JavaLaunchCommand {
        program: "java".to_owned(),
        args,
        current_dir: version_dir.to_path_buf(),
    })
}

fn game_launch_arguments(version_data: &MinecraftVersionData) -> Result<Vec<String>> {
    if let Some(arguments) = version_data.arguments.as_ref() {
        let game_arguments = collect_launch_arguments(&arguments.game)?;
        if !game_arguments.is_empty() || version_data.minecraft_arguments.is_none() {
            return Ok(game_arguments);
        }
    }

    if let Some(arguments) = version_data.minecraft_arguments.as_deref() {
        return Ok(arguments
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>());
    }

    bail!("installed Minecraft version does not include launch arguments");
}

fn build_classpath(
    launcher_root: &Path,
    client_jar: &Path,
    libraries: &[MinecraftLibrary],
) -> Result<String> {
    let os = OperatingSystem::current()?;
    let mut classpath = Vec::new();

    for library in libraries {
        if !allowed_by_rules(library.rules.as_deref(), os) {
            continue;
        }
        let Some(path) = library_artifact_path(library)? else {
            continue;
        };

        let library_path = launcher_root.join("libraries").join(path);
        if !library_path.is_file() {
            bail!(
                "installed Minecraft library is missing at `{}`",
                library_path.display()
            );
        }
        classpath.push(library_path);
    }

    classpath.push(client_jar.to_path_buf());
    join_classpath(classpath)
}

fn join_classpath(paths: Vec<PathBuf>) -> Result<String> {
    let paths = paths.into_iter().map(OsString::from).collect::<Vec<_>>();
    std::env::join_paths(paths)
        .context("failed to join Minecraft classpath")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("Minecraft classpath contains non-UTF-8 paths"))
}

struct LaunchArgumentContext<'a> {
    launcher_root: &'a Path,
    version_dir: &'a Path,
    version_id: &'a str,
    username: &'a str,
    classpath: &'a str,
    client_jar: &'a Path,
    asset_index_name: &'a str,
    version_type: &'a str,
    offline_uuid: String,
}

fn collect_launch_arguments(arguments: &[MinecraftArgument]) -> Result<Vec<String>> {
    let os = OperatingSystem::current()?;
    let mut collected = Vec::new();

    for argument in arguments {
        if !argument.allowed(os) {
            continue;
        }
        argument.extend_values(&mut collected);
    }

    Ok(collected)
}

fn launch_arguments_supply_classpath(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| {
        argument == "-cp"
            || argument == "-classpath"
            || argument.contains("${classpath}")
            || argument.contains("${primary_jar}")
    })
}

fn substitute_launch_argument(argument: &str, context: &LaunchArgumentContext<'_>) -> String {
    let libraries_dir = context.launcher_root.join("libraries");
    let assets_root = context.launcher_root.join("assets");
    let natives_dir = context.version_dir.join("natives");
    let game_directory = context.version_dir;
    let separator = if cfg!(windows) { ";" } else { ":" };

    let replacements = [
        ("${natives_directory}", path_to_string(&natives_dir)),
        ("${launcher_name}", DEFAULT_CLI_NAME.to_owned()),
        ("${launcher_version}", env!("CARGO_PKG_VERSION").to_owned()),
        ("${classpath}", context.classpath.to_owned()),
        ("${classpath_separator}", separator.to_owned()),
        ("${primary_jar}", path_to_string(context.client_jar)),
        ("${library_directory}", path_to_string(&libraries_dir)),
        ("${game_directory}", path_to_string(game_directory)),
        ("${auth_player_name}", context.username.to_owned()),
        ("${version_name}", context.version_id.to_owned()),
        ("${assets_root}", path_to_string(&assets_root)),
        ("${assets_index_name}", context.asset_index_name.to_owned()),
        ("${auth_uuid}", context.offline_uuid.clone()),
        ("${auth_access_token}", "0".to_owned()),
        ("${user_type}", "legacy".to_owned()),
        ("${version_type}", context.version_type.to_owned()),
        ("${user_properties}", "{}".to_owned()),
        ("${clientid}", String::new()),
        ("${auth_xuid}", String::new()),
    ];

    let mut substituted = argument.to_owned();
    for (token, value) in replacements {
        substituted = substituted.replace(token, &value);
    }
    substituted
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn offline_player_uuid(username: &str) -> String {
    use std::fmt::Write as _;

    let mut hash = Md5::digest(format!("OfflinePlayer:{username}").as_bytes());
    hash[6] = (hash[6] & 0x0f) | 0x30;
    hash[8] = (hash[8] & 0x3f) | 0x80;
    let mut uuid = String::with_capacity(32);
    for byte in hash {
        write!(&mut uuid, "{byte:02x}").expect("writing to String cannot fail");
    }
    uuid
}

pub fn install_minecraft_version(
    launcher_root: &Path,
    versions_folder: &Path,
    requested_version: &str,
) -> Result<InstalledVersion> {
    install_minecraft_version_with_alias(launcher_root, versions_folder, requested_version, None)
}

pub fn install_minecraft_version_with_alias(
    launcher_root: &Path,
    versions_folder: &Path,
    requested_version: &str,
    alias: Option<&str>,
) -> Result<InstalledVersion> {
    let mut downloader = HttpDownloader::new();
    install_minecraft_version_with_downloader(
        launcher_root,
        versions_folder,
        requested_version,
        alias,
        &mut downloader,
    )
}

fn install_minecraft_version_with_downloader(
    launcher_root: &Path,
    versions_folder: &Path,
    requested_version: &str,
    alias: Option<&str>,
    downloader: &mut impl Downloader,
) -> Result<InstalledVersion> {
    let manifest = downloader.download_string(MINECRAFT_VERSION_MANIFEST_URL)?;
    let manifest = parse_minecraft_version_manifest(&manifest)?;
    let selected = resolve_minecraft_version(&manifest, requested_version)?;
    let version_id = selected.id.clone();
    let version_url = selected.url.clone();
    validate_path_segment(&version_id, "resolved version id")?;
    let install_name = alias.unwrap_or(DEFAULT_INSTALL_ALIAS);
    validate_path_segment(install_name, "install alias")?;

    let version_dir = versions_folder.join(&version_id).join(install_name);
    if version_dir
        .try_exists()
        .with_context(|| format!("failed to inspect `{}`", version_dir.display()))?
    {
        bail!(
            "Minecraft version `{version_id}` with alias `{install_name}` is already installed at `{}`",
            version_dir.display()
        );
    }

    let fabric_loader_version = resolve_fabric_loader_version(downloader, &version_id)?;
    let version_data_json = downloader.download_string(&version_url)?;
    let fabric_profile_url = fabric_profile_url(&version_id, &fabric_loader_version);
    let fabric_profile_json = downloader.download_string(&fabric_profile_url)?;
    let installed_version_json =
        merge_minecraft_and_fabric_version_data(&version_data_json, &fabric_profile_json)?;
    let version_data = parse_minecraft_version_data(&installed_version_json)?;

    fs::create_dir_all(&version_dir)
        .with_context(|| format!("failed to create `{}`", version_dir.display()))?;
    ensure_version_mods_dir(&version_dir)?;
    fs::write(
        version_dir.join(format!("{install_name}.json")),
        &installed_version_json,
    )
    .with_context(|| format!("failed to write version data for `{version_id}`"))?;

    let client = version_data
        .downloads
        .client
        .context("selected Minecraft version does not include a client download")?;
    downloader.download_to_path(
        &client.url,
        &version_dir.join(format!("{install_name}.jar")),
    )?;

    if let Some(asset_index) = version_data.asset_index {
        install_assets(launcher_root, &asset_index, downloader)?;
    }

    install_libraries(
        launcher_root,
        &version_dir,
        version_data.libraries,
        downloader,
    )?;

    Ok(InstalledVersion {
        id: version_id,
        alias: alias.map(str::to_owned),
    })
}

fn ensure_version_mods_dir(version_dir: &Path) -> Result<PathBuf> {
    let mods_dir = version_dir.join(MODS_FOLDER_NAME);
    fs::create_dir_all(&mods_dir)
        .with_context(|| format!("failed to create `{}`", mods_dir.display()))?;
    Ok(mods_dir)
}

fn merge_minecraft_and_fabric_version_data(
    minecraft_version_data: &str,
    fabric_profile: &str,
) -> Result<String> {
    let mut merged: Value = serde_json::from_str(minecraft_version_data)
        .context("failed to parse Minecraft version data")?;
    let fabric: Value =
        serde_json::from_str(fabric_profile).context("failed to parse Fabric loader profile")?;

    copy_json_field(&mut merged, &fabric, "mainClass")?;
    append_json_array_field(&mut merged, &fabric, "libraries")?;
    append_fabric_launch_arguments(&mut merged, &fabric)?;

    serde_json::to_string_pretty(&merged).context("failed to serialize merged Fabric version data")
}

fn copy_json_field(target: &mut Value, source: &Value, key: &str) -> Result<()> {
    let Some(value) = source.get(key) else {
        return Ok(());
    };
    let target = target
        .as_object_mut()
        .context("Minecraft version data must be a JSON object")?;
    target.insert(key.to_owned(), value.clone());
    Ok(())
}

fn append_json_array_field(target: &mut Value, source: &Value, key: &str) -> Result<()> {
    let Some(source_value) = source.get(key) else {
        return Ok(());
    };
    let source_values = source_value
        .as_array()
        .with_context(|| format!("Fabric loader profile `{key}` must be an array"))?;
    if source_values.is_empty() {
        return Ok(());
    }

    let target = target
        .as_object_mut()
        .context("Minecraft version data must be a JSON object")?;
    let target_value = target
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let target_values = target_value
        .as_array_mut()
        .with_context(|| format!("Minecraft version data `{key}` must be an array"))?;
    target_values.extend(source_values.iter().cloned());
    Ok(())
}

fn append_fabric_launch_arguments(target: &mut Value, fabric: &Value) -> Result<()> {
    let Some(arguments) = fabric.get("arguments") else {
        return Ok(());
    };
    let arguments = arguments
        .as_object()
        .context("Fabric loader profile `arguments` must be a JSON object")?;
    if arguments.is_empty() {
        return Ok(());
    }

    let target_arguments = ensure_json_object_field(target, "arguments")?;
    for key in ["jvm", "game"] {
        append_argument_values(target_arguments, arguments, key)?;
    }
    Ok(())
}

fn ensure_json_object_field<'a>(
    target: &'a mut Value,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    let target = target
        .as_object_mut()
        .context("Minecraft version data must be a JSON object")?;
    let value = target
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    value
        .as_object_mut()
        .with_context(|| format!("Minecraft version data `{key}` must be a JSON object"))
}

fn append_argument_values(
    target_arguments: &mut Map<String, Value>,
    source_arguments: &Map<String, Value>,
    key: &str,
) -> Result<()> {
    let Some(source_value) = source_arguments.get(key) else {
        return Ok(());
    };
    let source_values = source_value
        .as_array()
        .with_context(|| format!("Fabric loader profile `arguments.{key}` must be an array"))?;
    if source_values.is_empty() {
        return Ok(());
    }

    let target_value = target_arguments
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let target_values = target_value
        .as_array_mut()
        .with_context(|| format!("Minecraft version data `arguments.{key}` must be an array"))?;
    target_values.extend(source_values.iter().cloned());
    Ok(())
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

fn library_artifact_path(library: &MinecraftLibrary) -> Result<Option<String>> {
    if let Some(path) = library
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.artifact.as_ref())
        .and_then(|artifact| artifact.path.as_deref())
    {
        return Ok(Some(path.to_owned()));
    }

    let Some(name) = library.name.as_deref() else {
        return Ok(None);
    };
    if library.url.is_none() {
        return Ok(None);
    }

    maven_artifact_path(name).map(Some)
}

fn library_artifact_download(library: &MinecraftLibrary) -> Result<Option<(String, String)>> {
    if let Some(artifact) = library
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.artifact.as_ref())
    {
        if let Some(path) = artifact.path.as_deref() {
            return Ok(Some((path.to_owned(), artifact.url.clone())));
        }
    }

    let Some(name) = library.name.as_deref() else {
        return Ok(None);
    };
    let Some(base_url) = library.url.as_deref() else {
        return Ok(None);
    };

    let path = maven_artifact_path(name)?;
    let url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    Ok(Some((path, url)))
}

fn maven_artifact_path(coordinates: &str) -> Result<String> {
    let parts = coordinates.split(':').collect::<Vec<_>>();
    if !(parts.len() == 3 || parts.len() == 4) || parts.iter().any(|part| part.is_empty()) {
        bail!("invalid Maven artifact coordinates `{coordinates}`");
    }

    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts
        .get(3)
        .map(|classifier| format!("-{classifier}"))
        .unwrap_or_default();

    Ok(format!(
        "{group}/{artifact}/{version}/{artifact}-{version}{classifier}.jar"
    ))
}

fn install_libraries(
    launcher_root: &Path,
    version_dir: &Path,
    libraries: Vec<MinecraftLibrary>,
    downloader: &mut impl Downloader,
) -> Result<()> {
    let os = OperatingSystem::current()?;
    let natives_dir = version_dir.join("natives");

    for library in libraries {
        if !allowed_by_rules(library.rules.as_deref(), os) {
            continue;
        }

        if let Some((path, url)) = library_artifact_download(&library)? {
            downloader.download_to_path(&url, &launcher_root.join("libraries").join(path))?;
        }

        if let Some(native) = native_library_download(&library, os) {
            let path = native.path.as_deref().with_context(|| {
                format!(
                    "native library `{}` does not include a download path",
                    library.name.as_deref().unwrap_or("unknown")
                )
            })?;
            let native_archive = launcher_root.join("libraries").join(path);
            downloader.download_to_path(&native.url, &native_archive)?;
            extract_native_archive(&native_archive, &natives_dir)?;
        }
    }

    Ok(())
}

fn native_library_download(
    library: &MinecraftLibrary,
    os: OperatingSystem,
) -> Option<&DownloadInfo> {
    let classifier = library
        .natives
        .as_ref()?
        .get(minecraft_os_name(os))?
        .replace("${arch}", native_arch_bits());
    library
        .downloads
        .as_ref()?
        .classifiers
        .as_ref()?
        .get(&classifier)
}

fn extract_native_archive(archive_path: &Path, natives_dir: &Path) -> Result<()> {
    fs::create_dir_all(natives_dir)
        .with_context(|| format!("failed to create `{}`", natives_dir.display()))?;
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open `{}`", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("failed to read native archive `{}`", archive_path.display()))?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).with_context(|| {
            format!(
                "failed to read entry {index} from `{}`",
                archive_path.display()
            )
        })?;
        if !is_native_library_name(entry.name()) {
            continue;
        }
        let Some(file_name) = Path::new(entry.name()).file_name() else {
            continue;
        };
        let output_path = natives_dir.join(file_name);
        let mut output = fs::File::create(&output_path)
            .with_context(|| format!("failed to create `{}`", output_path.display()))?;
        io::copy(&mut entry, &mut output)
            .with_context(|| format!("failed to write `{}`", output_path.display()))?;
    }

    Ok(())
}

fn is_native_library_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".dll")
        || lower.ends_with(".so")
        || lower.ends_with(".dylib")
        || lower.ends_with(".jnilib")
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

fn allowed_by_rules(rules: Option<&[MinecraftRule]>, os: OperatingSystem) -> bool {
    let Some(rules) = rules else {
        return true;
    };

    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule, os) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn rule_matches(rule: &MinecraftRule, os: OperatingSystem) -> bool {
    if let Some(rule_os) = rule.os.as_ref() {
        if let Some(name) = rule_os.name.as_deref() {
            if name != minecraft_os_name(os) {
                return false;
            }
        }
        if let Some(arch) = rule_os.arch.as_deref() {
            if arch != std::env::consts::ARCH {
                return false;
            }
        }
        if rule_os.version.is_some() {
            return false;
        }
    }

    if let Some(features) = rule.features.as_ref() {
        if features.values().any(|enabled| *enabled) {
            return false;
        }
    }

    true
}

fn minecraft_os_name(os: OperatingSystem) -> &'static str {
    match os {
        OperatingSystem::Linux => "linux",
        OperatingSystem::Macos => "osx",
        OperatingSystem::Windows => "windows",
    }
}

fn native_arch_bits() -> &'static str {
    if std::env::consts::ARCH.contains("64") {
        "64"
    } else {
        "32"
    }
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
        let content_length = response
            .header("Content-Length")
            .and_then(|value| value.parse::<u64>().ok());
        let progress = download_progress_bar(path, content_length);
        let partial_path = partial_download_path(path);
        match fs::remove_file(&partial_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                progress.finish_and_clear();
                return Err(error)
                    .with_context(|| format!("failed to remove `{}`", partial_path.display()));
            }
        }

        let mut reader = progress.wrap_read(response.into_reader());
        let result = (|| -> Result<()> {
            let mut file = fs::File::create(&partial_path)
                .with_context(|| format!("failed to create `{}`", partial_path.display()))?;
            io::copy(&mut reader, &mut file)
                .with_context(|| format!("failed to write `{}`", partial_path.display()))?;
            file.flush()
                .with_context(|| format!("failed to flush `{}`", partial_path.display()))?;
            fs::rename(&partial_path, path).with_context(|| {
                format!(
                    "failed to move `{}` to `{}`",
                    partial_path.display(),
                    path.display()
                )
            })?;
            Ok(())
        })();
        progress.finish_and_clear();

        if result.is_err() {
            let _ = fs::remove_file(&partial_path);
        }

        result
    }
}

fn download_progress_bar(path: &Path, content_length: Option<u64>) -> ProgressBar {
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    let label = truncate_progress_label(label);

    match content_length {
        Some(total) => {
            let progress = ProgressBar::new(total);
            progress.set_style(progress_bar_style());
            progress.set_message(label);
            progress
        }
        None => {
            let progress = ProgressBar::new_spinner();
            progress.set_style(download_spinner_style());
            progress.enable_steady_tick(Duration::from_millis(100));
            progress.set_message(label);
            progress
        }
    }
}

fn truncate_progress_label(label: &str) -> String {
    const MAX_LABEL_WIDTH: usize = 32;

    if label.chars().count() <= MAX_LABEL_WIDTH {
        return label.to_owned();
    }

    format!(
        "{}...",
        label.chars().take(MAX_LABEL_WIDTH - 3).collect::<String>()
    )
}

fn partial_download_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "download".into());
    file_name.push(".part");
    path.with_file_name(file_name)
}

fn progress_bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{msg:32} [{bar:40}] {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>12} ETA {eta}",
    )
    .expect("valid progress bar template")
    .progress_chars("=>-")
}

fn download_spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner} {msg:32} {bytes:>10} {bytes_per_sec:>12}")
        .expect("valid progress spinner template")
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
    #[serde(default)]
    arguments: Option<MinecraftLaunchArguments>,
    #[serde(rename = "assetIndex")]
    asset_index: Option<AssetIndex>,
    downloads: MinecraftDownloads,
    #[serde(default)]
    libraries: Vec<MinecraftLibrary>,
    #[serde(rename = "mainClass")]
    main_class: Option<String>,
    #[serde(rename = "minecraftArguments")]
    minecraft_arguments: Option<String>,
    #[serde(rename = "type")]
    version_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MinecraftLaunchArguments {
    #[serde(default)]
    game: Vec<MinecraftArgument>,
    #[serde(default)]
    jvm: Vec<MinecraftArgument>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MinecraftArgument {
    Value(ArgumentValue),
    Conditional {
        rules: Vec<MinecraftRule>,
        value: ArgumentValue,
    },
}

impl MinecraftArgument {
    fn allowed(&self, os: OperatingSystem) -> bool {
        match self {
            Self::Value(_) => true,
            Self::Conditional { rules, .. } => allowed_by_rules(Some(rules.as_slice()), os),
        }
    }

    fn extend_values(&self, values: &mut Vec<String>) {
        match self {
            Self::Value(value) | Self::Conditional { value, .. } => value.extend(values),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ArgumentValue {
    One(String),
    Many(Vec<String>),
}

impl ArgumentValue {
    fn extend(&self, values: &mut Vec<String>) {
        match self {
            Self::One(value) => values.push(value.clone()),
            Self::Many(items) => values.extend(items.iter().cloned()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct MinecraftDownloads {
    client: Option<DownloadInfo>,
}

#[derive(Debug, Deserialize)]
struct MinecraftLibrary {
    downloads: Option<LibraryDownloads>,
    name: Option<String>,
    natives: Option<HashMap<String, String>>,
    rules: Option<Vec<MinecraftRule>>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LibraryDownloads {
    artifact: Option<DownloadInfo>,
    classifiers: Option<HashMap<String, DownloadInfo>>,
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

#[derive(Debug, Deserialize)]
struct MinecraftRule {
    action: String,
    features: Option<HashMap<String, bool>>,
    os: Option<MinecraftRuleOs>,
}

#[derive(Debug, Deserialize)]
struct MinecraftRuleOs {
    arch: Option<String>,
    name: Option<String>,
    version: Option<String>,
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

fn validate_path_segment(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.trim() != value {
        bail!("{label} cannot contain leading or trailing whitespace");
    }
    if value.contains('/') || value.contains('\\') {
        bail!("{label} must be a single path segment");
    }

    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => bail!("{label} must be a single path segment"),
    }
}

fn validate_username(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("username cannot be empty");
    }
    if value.trim() != value {
        bail!("username cannot contain leading or trailing whitespace");
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        bail!("username can only contain ASCII letters, numbers, and underscores");
    }
    Ok(())
}

fn write_versions(versions: &[FabricMinecraftVersion], stdout: &mut impl Write) -> Result<()> {
    for version in versions {
        writeln!(
            stdout,
            "Minecraft {} - Fabric {}",
            version.minecraft_version, version.fabric_version
        )
        .context("failed to write version output")?;
    }
    Ok(())
}

fn write_install_output(installed: &InstalledVersion, stdout: &mut impl Write) -> Result<()> {
    match installed.alias.as_deref() {
        Some(alias) => writeln!(stdout, "Installed {} as {alias}", installed.id),
        None => writeln!(stdout, "Installed {}", installed.id),
    }
    .context("failed to write install output")
}

fn write_run_output(launched: &LaunchedVersion, stdout: &mut impl Write) -> Result<()> {
    match launched.alias.as_deref() {
        Some(alias) => writeln!(
            stdout,
            "Ran {} as {} with alias {alias}",
            launched.id, launched.username
        ),
        None => writeln!(stdout, "Ran {} as {}", launched.id, launched.username),
    }
    .context("failed to write run output")
}

fn write_usage(cli_name: &str, stdout: &mut impl Write) -> Result<()> {
    write!(stdout, "{}", usage_text(cli_name)).context("failed to write usage")
}

fn usage_text(cli_name: &str) -> String {
    format!(
        "Usage: {cli_name} versions\n       {cli_name} install {{version}}|latest [--alias {{alias}}]\n       {cli_name} run {{version}} [--alias {{alias}}] --username {{username}}\n       {cli_name} configure-path\n       {cli_name} unset-path\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_env_for<'a>(
        home: &'a Path,
        path: Option<&'a Path>,
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| match key {
            "HOME" => Some(home.display().to_string()),
            "APPDATA" => Some("C:/Users/Player/AppData/Roaming".to_owned()),
            "PATH" => path.map(|path| path.display().to_string()),
            _ => None,
        }
    }

    fn launcher_root_for_home(home: &Path) -> PathBuf {
        let env = test_env_for(home, None);
        COMPILED_SETTINGS
            .launcher_path_for(OperatingSystem::current().unwrap(), &env)
            .unwrap()
    }

    #[test]
    fn compiled_settings_expand_launcher_path_from_recipe() {
        let home = Path::new("/home/player");
        let env = test_env_for(home, None);

        let path = COMPILED_SETTINGS
            .launcher_path_for(OperatingSystem::Linux, &env)
            .unwrap();

        assert_eq!(path, PathBuf::from("/home/player/.config/clear-launcher"));
    }

    #[test]
    fn compiled_settings_include_cli_name_from_recipe() {
        assert_eq!(COMPILED_SETTINGS.cli_name(), DEFAULT_CLI_NAME);
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
    fn parses_fabric_game_versions_in_api_order() {
        let versions = parse_fabric_game_versions(
            r#"
[
  {
    "version": "1.20.4",
    "stable": true
  },
  {
    "version": "23w13a_or_b",
    "stable": false
  }
]
"#,
        )
        .unwrap();

        assert_eq!(versions, vec!["1.20.4", "23w13a_or_b"]);
    }

    #[test]
    fn versions_command_uses_compiled_settings_and_fetches_from_api() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            repo.path().join("settings.yml"),
            "this: is not used at runtime",
        )
        .unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec!["versions".to_owned()],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || {
                Ok(vec![
                    FabricMinecraftVersion {
                        minecraft_version: "1.20.4".to_owned(),
                        fabric_version: "0.19.3".to_owned(),
                    },
                    FabricMinecraftVersion {
                        minecraft_version: "23w13a_or_b".to_owned(),
                        fabric_version: "0.19.3".to_owned(),
                    },
                ])
            },
            |_, _, _| unreachable!("versions command should not install versions"),
            |_, _, _| unreachable!("versions command should not run versions"),
            || unreachable!("versions command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "Minecraft 1.20.4 - Fabric 0.19.3\nMinecraft 23w13a_or_b - Fabric 0.19.3\n"
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Using compiled launcher settings"));
        assert!(logs.contains("Fetching Fabric-supported Minecraft versions"));
        assert!(logs.contains("Finished listing versions"));
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
    fn install_command_uses_compiled_settings_and_installs_requested_version() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        fs::create_dir_all(&cwd).unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec!["install".to_owned(), "1.18".to_owned()],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("install command should not fetch Fabric versions"),
            |root, versions, requested| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(versions, versions_folder.as_path());
                assert_eq!(
                    requested,
                    &InstallRequest {
                        requested_version: "1.18".to_owned(),
                        alias: None,
                    }
                );
                assert!(root.is_dir());
                Ok(InstalledVersion {
                    id: "1.18.2".to_owned(),
                    alias: None,
                })
            },
            |_, _, _| unreachable!("install command should not run versions"),
            || unreachable!("install command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(String::from_utf8(stdout).unwrap(), "Installed 1.18.2\n");
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Preparing install for requested Minecraft version `1.18`"));
        assert!(logs.contains("Using versions folder"));
        assert!(logs.contains("Finished installing `1.18.2`"));
    }

    #[test]
    fn install_command_accepts_alias() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        fs::create_dir_all(&cwd).unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec![
                "install".to_owned(),
                "latest".to_owned(),
                "--alias".to_owned(),
                "survival".to_owned(),
            ],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("install command should not fetch Fabric versions"),
            |root, versions, requested| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(versions, versions_folder.as_path());
                assert_eq!(
                    requested,
                    &InstallRequest {
                        requested_version: "latest".to_owned(),
                        alias: Some("survival".to_owned()),
                    }
                );
                Ok(InstalledVersion {
                    id: "1.20.4".to_owned(),
                    alias: requested.alias.clone(),
                })
            },
            |_, _, _| unreachable!("install command should not run versions"),
            || unreachable!("install command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "Installed 1.20.4 as survival\n"
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Using install alias `survival`"));
    }

    #[test]
    fn run_command_uses_compiled_settings_and_runs_requested_version() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        fs::create_dir_all(&cwd).unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec![
                "run".to_owned(),
                "1.18".to_owned(),
                "--alias".to_owned(),
                "survival".to_owned(),
                "--username".to_owned(),
                "Player_1".to_owned(),
            ],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("run command should not fetch Fabric versions"),
            |_, _, _| unreachable!("run command should not install versions"),
            |root, versions, requested| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(versions, versions_folder.as_path());
                assert_eq!(
                    requested,
                    &RunRequest {
                        requested_version: "1.18".to_owned(),
                        alias: Some("survival".to_owned()),
                        username: "Player_1".to_owned(),
                    }
                );
                assert!(root.is_dir());
                Ok(LaunchedVersion {
                    id: "1.18.2".to_owned(),
                    alias: requested.alias.clone(),
                    username: requested.username.clone(),
                })
            },
            || unreachable!("run command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "Ran 1.18.2 as Player_1 with alias survival\n"
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Preparing run for requested Minecraft version `1.18`"));
        assert!(logs.contains("Using run alias `survival`"));
        assert!(logs.contains("Using offline username `Player_1`"));
        assert!(logs.contains("Finished running `1.18.2` as `Player_1`"));
    }

    #[test]
    fn run_builds_offline_java_launch_command_from_installed_version() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        let version_dir = versions_folder.join("1.20.4/default");
        let library_path = launcher_root.join("libraries/org/example/lib/1.0/lib-1.0.jar");
        fs::create_dir_all(&version_dir).unwrap();
        fs::create_dir_all(library_path.parent().unwrap()).unwrap();
        fs::write(version_dir.join("default.jar"), "client").unwrap();
        fs::write(&library_path, "library").unwrap();
        fs::write(
            version_dir.join("default.json"),
            r#"
{
  "type": "release",
  "mainClass": "net.minecraft.client.main.Main",
  "assetIndex": {
    "id": "1.20",
    "url": "https://example.test/assets/1.20.json"
  },
  "arguments": {
    "jvm": [
      "-Djava.library.path=${natives_directory}",
      "-Dminecraft.launcher.brand=${launcher_name}",
      "-cp",
      "${classpath}"
    ],
    "game": [
      "--username",
      "${auth_player_name}",
      "--uuid",
      "${auth_uuid}",
      "--accessToken",
      "${auth_access_token}",
      "--version",
      "${version_name}",
      "--gameDir",
      "${game_directory}",
      "--assetsDir",
      "${assets_root}",
      "--assetIndex",
      "${assets_index_name}",
      "--userType",
      "${user_type}",
      {
        "rules": [
          {
            "action": "allow",
            "features": {
              "is_demo_user": true
            }
          }
        ],
        "value": "--demo"
      }
    ]
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
"#,
        )
        .unwrap();

        let mut downloader = FakeDownloader::new([(
            MINECRAFT_VERSION_MANIFEST_URL,
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
      "url": "https://example.test/1.20.4.json"
    }
  ]
}
"#,
        )]);
        let mut launcher = FakeJavaLauncher::default();
        let request = RunRequest {
            requested_version: "latest".to_owned(),
            alias: None,
            username: "Player_1".to_owned(),
        };

        let launched = run_minecraft_version_offline_with_services(
            &launcher_root,
            &versions_folder,
            &request,
            &mut downloader,
            &mut launcher,
        )
        .unwrap();

        assert_eq!(
            launched,
            LaunchedVersion {
                id: "1.20.4".to_owned(),
                alias: None,
                username: "Player_1".to_owned(),
            }
        );
        let command = launcher.command.unwrap();
        assert_eq!(command.program, "java");
        assert_eq!(command.current_dir, version_dir);
        assert!(
            command
                .args
                .contains(&"net.minecraft.client.main.Main".to_owned())
        );
        assert!(command.args.contains(&"Player_1".to_owned()));
        assert!(command.args.contains(&offline_player_uuid("Player_1")));
        assert!(command.args.contains(&"0".to_owned()));
        assert!(command.args.contains(&"legacy".to_owned()));
        assert!(!command.args.contains(&"--demo".to_owned()));
        let classpath_index = command
            .args
            .iter()
            .position(|argument| argument == "-cp")
            .unwrap()
            + 1;
        let classpath = &command.args[classpath_index];
        assert!(classpath.contains("org/example/lib/1.0/lib-1.0.jar"));
        assert!(classpath.contains("versions/1.20.4/default/default.jar"));
        let game_dir_index = command
            .args
            .iter()
            .position(|argument| argument == "--gameDir")
            .unwrap()
            + 1;
        assert_eq!(command.args[game_dir_index], path_to_string(&version_dir));
        assert!(version_dir.join(MODS_FOLDER_NAME).is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn configure_and_unset_path_manage_symlink_config() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let bin_dir = repo.path().join("bin");
        let exe_path = repo
            .path()
            .join("target")
            .join("debug")
            .join("clear-launcher");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(exe_path.parent().unwrap()).unwrap();
        fs::write(&exe_path, "binary").unwrap();

        let env = test_env_for(&home, Some(&bin_dir));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec!["configure-path".to_owned()],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("configure-path should not fetch Fabric versions"),
            |_, _, _| unreachable!("configure-path should not install versions"),
            |_, _, _| unreachable!("configure-path should not run versions"),
            || Ok(exe_path.clone()),
        )
        .unwrap();

        let cli_name = COMPILED_SETTINGS.cli_name();
        let symlink_path = bin_dir.join(cli_name);
        assert_eq!(fs::read_link(&symlink_path).unwrap(), exe_path);
        assert_eq!(
            load_launcher_config(&launcher_root.join(CONFIG_FILE_NAME))
                .unwrap()
                .path_symlink,
            Some(symlink_path.clone())
        );
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("Configured {cli_name} at {}\n", symlink_path.display())
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Configuring CLI path"));
        assert!(logs.contains("Configured CLI path"));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec!["unset-path".to_owned()],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("unset-path should not fetch Fabric versions"),
            |_, _, _| unreachable!("unset-path should not install versions"),
            |_, _, _| unreachable!("unset-path should not run versions"),
            || unreachable!("unset-path should not inspect the current executable"),
        )
        .unwrap();

        assert!(!symlink_path.exists());
        assert_eq!(
            load_launcher_config(&launcher_root.join(CONFIG_FILE_NAME))
                .unwrap()
                .path_symlink,
            None
        );
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("Unset {}\n", symlink_path.display())
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Unsetting CLI path"));
        assert!(logs.contains("Removed configured CLI path"));
    }

    #[test]
    fn installs_minecraft_version_files_from_manifest() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        let asset_hash = "abcdef0123456789abcdef0123456789abcdef01";
        let loader_versions_url = fabric_loader_versions_for_minecraft_url("1.18.2");
        let fabric_profile_endpoint = fabric_profile_url("1.18.2", "0.19.3");
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
        let fabric_profile = r#"
{
  "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
  "arguments": {
    "jvm": [
      "-DFabricMcEmu= net.minecraft.client.main.Main "
    ],
    "game": []
  },
  "libraries": [
    {
      "name": "net.fabricmc:fabric-loader:0.19.3",
      "url": "https://maven.fabricmc.net/"
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
            (
                loader_versions_url.as_str(),
                r#"
[
  {
    "loader": {
      "version": "0.19.2",
      "stable": false
    }
  },
  {
    "loader": {
      "version": "0.19.3",
      "stable": true
    }
  }
]
"#,
            ),
            ("https://example.test/1.18.2.json", version_data),
            (fabric_profile_endpoint.as_str(), fabric_profile),
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

        let installed = install_minecraft_version_with_downloader(
            &launcher_root,
            &versions_folder,
            "latest",
            None,
            &mut downloader,
        )
        .unwrap();

        assert_eq!(installed.id, "1.18.2");
        let installed_data: Value = serde_json::from_str(
            &fs::read_to_string(versions_folder.join("1.18.2/default/default.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            installed_data
                .get("mainClass")
                .and_then(Value::as_str)
                .unwrap(),
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        assert_eq!(
            installed_data
                .pointer("/downloads/client/url")
                .and_then(Value::as_str)
                .unwrap(),
            "https://example.test/client.jar"
        );
        assert!(
            installed_data
                .get("libraries")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .any(|library| library.get("name").and_then(Value::as_str)
                    == Some("net.fabricmc:fabric-loader:0.19.3"))
        );
        assert_eq!(
            fs::read(versions_folder.join("1.18.2/default/default.jar")).unwrap(),
            b"https://example.test/client.jar"
        );
        assert!(launcher_root.join("versions/1.18.2/default").exists());
        assert!(launcher_root.join("versions/1.18.2/default/mods").is_dir());
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
        assert_eq!(
            fs::read(
                launcher_root
                    .join("libraries/net/fabricmc/fabric-loader/0.19.3/fabric-loader-0.19.3.jar")
            )
            .unwrap(),
            b"https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.19.3/fabric-loader-0.19.3.jar"
        );
    }

    #[test]
    fn installs_minecraft_version_files_under_alias() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        let loader_versions_url = fabric_loader_versions_for_minecraft_url("1.20.4");
        let fabric_profile_endpoint = fabric_profile_url("1.20.4", "0.19.3");
        let version_data = r#"
{
  "downloads": {
    "client": {
      "url": "https://example.test/client.jar"
    }
  }
}
"#;
        let fabric_profile = r#"
{
  "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
  "libraries": [
    {
      "name": "net.fabricmc:fabric-loader:0.19.3",
      "url": "https://maven.fabricmc.net/"
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
    "release": "1.20.4",
    "snapshot": "23w13a_or_b"
  },
  "versions": [
    {
      "id": "1.20.4",
      "type": "release",
      "url": "https://example.test/1.20.4.json"
    }
  ]
}
"#,
            ),
            (
                loader_versions_url.as_str(),
                r#"
[
  {
    "loader": {
      "version": "0.19.3",
      "stable": true
    }
  }
]
"#,
            ),
            ("https://example.test/1.20.4.json", version_data),
            (fabric_profile_endpoint.as_str(), fabric_profile),
        ]);

        let installed = install_minecraft_version_with_downloader(
            &launcher_root,
            &versions_folder,
            "latest",
            Some("survival"),
            &mut downloader,
        )
        .unwrap();

        assert_eq!(
            installed,
            InstalledVersion {
                id: "1.20.4".to_owned(),
                alias: Some("survival".to_owned()),
            }
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                &fs::read_to_string(versions_folder.join("1.20.4/survival/survival.json")).unwrap()
            )
            .unwrap()
            .get("mainClass")
            .and_then(Value::as_str),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        assert_eq!(
            fs::read(versions_folder.join("1.20.4/survival/survival.jar")).unwrap(),
            b"https://example.test/client.jar"
        );
        assert!(versions_folder.join("1.20.4/survival/mods").is_dir());
        assert!(
            launcher_root
                .join("libraries/net/fabricmc/fabric-loader/0.19.3/fabric-loader-0.19.3.jar")
                .is_file()
        );
        assert!(!versions_folder.join("1.20.4/default").exists());
    }

    #[test]
    fn install_aborts_when_alias_target_already_exists() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        fs::create_dir_all(versions_folder.join("1.20.4/survival")).unwrap();

        let mut downloader = FakeDownloader::new([(
            MINECRAFT_VERSION_MANIFEST_URL,
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
      "url": "https://example.test/1.20.4.json"
    }
  ]
}
"#,
        )]);

        let error = install_minecraft_version_with_downloader(
            &launcher_root,
            &versions_folder,
            "latest",
            Some("survival"),
            &mut downloader,
        )
        .unwrap_err();

        assert!(error.to_string().contains("already installed"));
    }

    struct FakeDownloader {
        strings: HashMap<String, String>,
    }

    #[derive(Default)]
    struct FakeJavaLauncher {
        command: Option<JavaLaunchCommand>,
    }

    impl JavaLauncher for FakeJavaLauncher {
        fn launch(&mut self, command: &JavaLaunchCommand) -> Result<()> {
            self.command = Some(command.clone());
            Ok(())
        }
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
