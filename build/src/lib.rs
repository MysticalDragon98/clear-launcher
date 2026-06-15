use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const DEFAULT_CLI_NAME: &str = "clear-launcher";
pub const SETTINGS_FILE: &str = "settings.yml";
pub const CONFIG_FILE_NAME: &str = "config.yml";
pub const BUILD_FOLDER_NAME: &str = "build";
pub const VERSIONS_FOLDER_NAME: &str = "versions";
pub const DEFAULT_INSTALL_ALIAS: &str = "default";
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
    stderr: &mut impl Write,
) -> Result<()> {
    execute_with_services(
        args,
        cwd,
        env_get,
        stdout,
        stderr,
        fetch_fabric_loader_versions,
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
    fetch_versions: impl FnOnce() -> Result<Vec<String>>,
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

            write_log(stderr, format_args!("Loading launcher settings"))?;
            let settings_context = load_settings_context(cwd)?;
            let launcher_root = settings_context
                .settings
                .launcher_path_for(OperatingSystem::current()?, env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            write_log(stderr, format_args!("Fetching Fabric loader versions"))?;
            let versions = fetch_versions()?;
            write_log(
                stderr,
                format_args!("Writing {} Fabric loader versions", versions.len()),
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
            write_log(stderr, format_args!("Loading launcher settings"))?;
            let settings_context = load_settings_context(cwd)?;
            let launcher_root = settings_context
                .settings
                .launcher_path_for(OperatingSystem::current()?, env_get)?;
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
            write_log(stderr, format_args!("Loading launcher settings"))?;
            let settings_context = load_settings_context(cwd)?;
            let launcher_root = settings_context
                .settings
                .launcher_path_for(OperatingSystem::current()?, env_get)?;
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
            write_log(stderr, format_args!("Loading launcher settings"))?;
            let settings_context = load_settings_context(cwd)?;
            let launcher_root = settings_context
                .settings
                .launcher_path_for(OperatingSystem::current()?, env_get)?;
            let cli_name = settings_context.settings.cli_name();
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
            write_log(stderr, format_args!("Loading launcher settings"))?;
            let settings_context = load_settings_context(cwd)?;
            let launcher_root = settings_context
                .settings
                .launcher_path_for(OperatingSystem::current()?, env_get)?;
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

pub fn load_settings(cwd: &Path) -> Result<Settings> {
    Ok(load_settings_context(cwd)?.settings)
}

struct SettingsContext {
    settings: Settings,
}

fn load_settings_context(cwd: &Path) -> Result<SettingsContext> {
    let path = find_settings_path(cwd).with_context(|| {
        format!(
            "`{SETTINGS_FILE}` was not found from `{}` or its parent directories",
            cwd.display()
        )
    })?;
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    let settings = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;

    Ok(SettingsContext { settings })
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

    let game_templates = if let Some(arguments) = version_data.arguments.as_ref() {
        collect_launch_arguments(&arguments.game)?
    } else if let Some(arguments) = version_data.minecraft_arguments.as_deref() {
        arguments
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        bail!("installed Minecraft version does not include launch arguments");
    };
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
        current_dir: launcher_root.to_path_buf(),
    })
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
        let Some(path) = library
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.artifact.as_ref())
            .and_then(|artifact| artifact.path.as_deref())
        else {
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
    let game_directory = context.launcher_root;
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

    let version_data_json = downloader.download_string(&version_url)?;
    let version_data = parse_minecraft_version_data(&version_data_json)?;

    fs::create_dir_all(&version_dir)
        .with_context(|| format!("failed to create `{}`", version_dir.display()))?;
    fs::write(
        version_dir.join(format!("{install_name}.json")),
        &version_data_json,
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

        if let Some(artifact) = library
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.artifact.as_ref())
        {
            if let Some(path) = artifact.path.as_deref() {
                downloader
                    .download_to_path(&artifact.url, &launcher_root.join("libraries").join(path))?;
            }
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

fn write_versions(versions: &[String], stdout: &mut impl Write) -> Result<()> {
    for version in versions {
        writeln!(stdout, "{version}").context("failed to write version output")?;
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
        let mut stderr = Vec::new();
        execute_with_services(
            vec!["versions".to_owned()],
            &cwd,
            &test_env,
            &mut stdout,
            &mut stderr,
            || Ok(vec!["0.19.3".to_owned(), "0.10.6+build.214".to_owned()]),
            |_, _, _| unreachable!("versions command should not install versions"),
            |_, _, _| unreachable!("versions command should not run versions"),
            || unreachable!("versions command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "0.19.3\n0.10.6+build.214\n"
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Loading launcher settings"));
        assert!(logs.contains("Fetching Fabric loader versions"));
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
    fn install_command_reads_settings_and_installs_requested_version() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            repo.path().join(SETTINGS_FILE),
            format!("launcher_path:\n  linux: \"{}\"\n", launcher_root.display()),
        )
        .unwrap();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec!["install".to_owned(), "1.18".to_owned()],
            &cwd,
            &test_env,
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
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            repo.path().join(SETTINGS_FILE),
            format!("launcher_path:\n  linux: \"{}\"\n", launcher_root.display()),
        )
        .unwrap();

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
            &test_env,
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
    fn run_command_reads_settings_and_runs_requested_version() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            repo.path().join(SETTINGS_FILE),
            format!("launcher_path:\n  linux: \"{}\"\n", launcher_root.display()),
        )
        .unwrap();

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
            &test_env,
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
        assert_eq!(command.current_dir, launcher_root);
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
    }

    #[cfg(unix)]
    #[test]
    fn configure_and_unset_path_manage_symlink_config() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let launcher_root = repo.path().join("launcher");
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
        fs::write(
            repo.path().join(SETTINGS_FILE),
            format!(
                "launcher_path:\n  linux: \"{}\"\ncli_name: custom-launcher\n",
                launcher_root.display()
            ),
        )
        .unwrap();

        let env = |key: &str| match key {
            "HOME" => Some("/home/player".to_owned()),
            "APPDATA" => Some("C:/Users/Player/AppData/Roaming".to_owned()),
            "PATH" => Some(bin_dir.display().to_string()),
            _ => None,
        };

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

        let symlink_path = bin_dir.join("custom-launcher");
        assert_eq!(fs::read_link(&symlink_path).unwrap(), exe_path);
        assert_eq!(
            load_launcher_config(&launcher_root.join(CONFIG_FILE_NAME))
                .unwrap()
                .path_symlink,
            Some(symlink_path.clone())
        );
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("Configured custom-launcher at {}\n", symlink_path.display())
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

        let installed = install_minecraft_version_with_downloader(
            &launcher_root,
            &versions_folder,
            "latest",
            None,
            &mut downloader,
        )
        .unwrap();

        assert_eq!(installed.id, "1.18.2");
        assert_eq!(
            fs::read_to_string(versions_folder.join("1.18.2/default/default.json")).unwrap(),
            version_data
        );
        assert_eq!(
            fs::read(versions_folder.join("1.18.2/default/default.jar")).unwrap(),
            b"https://example.test/client.jar"
        );
        assert!(launcher_root.join("versions/1.18.2/default").exists());
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

    #[test]
    fn installs_minecraft_version_files_under_alias() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        let version_data = r#"
{
  "downloads": {
    "client": {
      "url": "https://example.test/client.jar"
    }
  }
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
            ("https://example.test/1.20.4.json", version_data),
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
            fs::read_to_string(versions_folder.join("1.20.4/survival/survival.json")).unwrap(),
            version_data
        );
        assert_eq!(
            fs::read(versions_folder.join("1.20.4/survival/survival.jar")).unwrap(),
            b"https://example.test/client.jar"
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
