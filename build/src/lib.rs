use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use serde_yaml::{Mapping, Value as YamlValue};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

pub const DEFAULT_CLI_NAME: &str = env!("CLEAR_LAUNCHER_CLI_NAME");
pub const DEFAULT_LINUX_LAUNCHER_PATH: &str = env!("CLEAR_LAUNCHER_LAUNCHER_PATH_LINUX");
pub const DEFAULT_MACOS_LAUNCHER_PATH: &str = env!("CLEAR_LAUNCHER_LAUNCHER_PATH_MACOS");
pub const DEFAULT_WINDOWS_LAUNCHER_PATH: &str = env!("CLEAR_LAUNCHER_LAUNCHER_PATH_WINDOWS");
pub const SOURCE_FOLDER: &str = env!("CLEAR_LAUNCHER_SOURCE_FOLDER");
pub const CONFIG_FILE_NAME: &str = "config.yml";
pub const MOD_CONFIG_FILE_NAME: &str = "mod.yml";
pub const BUILD_FOLDER_NAME: &str = "build";
pub const TEST_MINECRAFT_FOLDER_NAME: &str = ".minecraft";
pub const VERSIONS_FOLDER_NAME: &str = "versions";
pub const MODS_FOLDER_NAME: &str = "mods";
pub const REMOTE_MANIFEST_FILE_NAME: &str = "remote.yml";
pub const DEFAULT_OPEN_ADDRESS: &str = "0.0.0.0:7878";
pub const DEFAULT_INSTALL_ALIAS: &str = "default";
pub const DEFAULT_EDITOR: &str = "code";
pub const DEFAULT_MOD_VERSION: &str = "1.0.0";
pub const DEFAULT_TEST_USERNAME: &str = "Player";
pub const FABRIC_LOADER_VERSIONS_URL: &str = "https://meta.fabricmc.net/v2/versions/loader";
pub const FABRIC_GAME_VERSIONS_URL: &str = "https://meta.fabricmc.net/v2/versions/game";
pub const FABRIC_YARN_VERSIONS_URL: &str = "https://meta.fabricmc.net/v2/versions/yarn";
pub const FABRIC_LOOM_MAVEN_METADATA_URL: &str =
    "https://maven.fabricmc.net/net/fabricmc/fabric-loom/maven-metadata.xml";
pub const FABRIC_MAVEN_URL: &str = "https://maven.fabricmc.net";
pub const GRADLE_CURRENT_VERSION_URL: &str = "https://services.gradle.org/versions/current";
pub const MINECRAFT_VERSION_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest.json";
pub const MODRINTH_SEARCH_URL: &str = "https://api.modrinth.com/v2/search";

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
    #[serde(
        rename = "path",
        alias = "path_symlink",
        skip_serializing_if = "Option::is_none"
    )]
    pub path_symlink: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mods_folder: Option<PathBuf>,
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

impl LauncherConfig {
    pub fn editor_command(&self) -> &str {
        self.editor
            .as_deref()
            .map(str::trim)
            .filter(|editor| !editor.is_empty())
            .unwrap_or(DEFAULT_EDITOR)
    }

    pub fn configured_mods_folder(&self, launcher_root: &Path) -> PathBuf {
        self.mods_folder
            .as_ref()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    launcher_root.join(path)
                }
            })
            .unwrap_or_else(|| launcher_root.join(MODS_FOLDER_NAME))
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
        |launcher_root, config, request| create_mod_project(launcher_root, config, request),
        |launcher_root, config, cwd, request| {
            build_mod_project(launcher_root, config, cwd, request)
        },
        |launcher_root, versions_folder, config, cwd, request| {
            install_mod_project(launcher_root, versions_folder, config, cwd, request)
        },
        |launcher_root, config, request| clone_mod_project(launcher_root, config, request),
        |launcher_root, config, cwd, request| test_mod_project(launcher_root, config, cwd, request),
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
    create_mod: impl FnOnce(&Path, &LauncherConfig, &CreateModRequest) -> Result<CreatedMod>,
    build_mod: impl FnOnce(&Path, &LauncherConfig, &Path, &BuildModRequest) -> Result<BuiltMod>,
    install_mod: impl FnOnce(
        &Path,
        &Path,
        &LauncherConfig,
        &Path,
        &InstallModRequest,
    ) -> Result<InstalledMod>,
    clone_mod: impl FnOnce(&Path, &LauncherConfig, &CloneModRequest) -> Result<ClonedMod>,
    test_mod: impl FnOnce(&Path, &LauncherConfig, &Path, &TestModRequest) -> Result<TestedMod>,
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
            if requested_version == "mod" {
                let install_request = parse_install_mod_request(args)?;

                match install_request.name.as_deref() {
                    Some(name) => {
                        write_log(stderr, format_args!("Preparing install for mod `{name}`"))?
                    }
                    None => write_log(stderr, format_args!("Preparing install for current mod"))?,
                }
                if let Some(alias) = install_request.alias.as_deref() {
                    write_log(stderr, format_args!("Using install alias `{alias}`"))?;
                } else {
                    write_log(
                        stderr,
                        format_args!("Using install alias `{DEFAULT_INSTALL_ALIAS}`"),
                    )?;
                }
                write_log(stderr, format_args!("Using compiled launcher settings"))?;
                let launcher_root = launcher_root_from_compiled_settings(env_get)?;
                write_log(
                    stderr,
                    format_args!("Ensuring launcher path {}", launcher_root.display()),
                )?;
                ensure_launcher_root(&launcher_root)?;
                let config_file = launcher_config_file(&launcher_root);
                let config = load_launcher_config(&config_file)?;
                let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
                write_log(
                    stderr,
                    format_args!("Using versions folder {}", versions_folder.display()),
                )?;
                write_log(
                    stderr,
                    format_args!(
                        "Using my mods folder {}",
                        config.configured_mods_folder(&launcher_root).display()
                    ),
                )?;
                let installed = install_mod(
                    &launcher_root,
                    &versions_folder,
                    &config,
                    cwd,
                    &install_request,
                )?;
                write_installed_mod_output(&installed, stdout)?;
                return write_log(
                    stderr,
                    format_args!(
                        "Finished installing mod `{}` into `{}`",
                        installed.name,
                        installed.destination.display()
                    ),
                );
            }
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
            let first = args.next().with_context(|| {
                format!(
                    "missing version or `--connect` for `run`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            let run_request = parse_run_request(first, args)?;

            if let Some(host) = run_request.connect.as_deref() {
                write_log(
                    stderr,
                    format_args!(
                        "Preparing remote run from `{host}` as `{}`",
                        run_request.name.as_deref().unwrap_or(DEFAULT_INSTALL_ALIAS)
                    ),
                )?;
            } else {
                write_log(
                    stderr,
                    format_args!(
                        "Preparing run for requested Minecraft version `{}`",
                        run_request
                            .requested_version
                            .as_deref()
                            .context("missing version for local run")?
                    ),
                )?;
            }
            if let Some(alias) = run_request.alias.as_deref() {
                write_log(stderr, format_args!("Using run alias `{alias}`"))?;
            }
            if let Some(open) = run_request.open.as_deref() {
                write_log(stderr, format_args!("Opening remote server on `{open}`"))?;
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
        Some("create") => {
            let target = args.next().with_context(|| {
                format!(
                    "missing target for `create`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            if target != "mod" {
                bail!(
                    "unknown create target `{target}`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                );
            }

            let name = args.next().with_context(|| {
                format!(
                    "missing name for `create mod`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            let create_request = parse_create_mod_request(name, args)?;

            write_log(
                stderr,
                format_args!("Preparing mod project `{}`", create_request.name),
            )?;
            match create_request.minecraft_version.as_deref() {
                Some(version) => write_log(
                    stderr,
                    format_args!("Using requested Minecraft version `{version}`"),
                )?,
                None => write_log(stderr, format_args!("Using latest Minecraft release"))?,
            }
            if let Some(fabric_version) = create_request.fabric_version.as_deref() {
                write_log(
                    stderr,
                    format_args!("Using requested Fabric loader `{fabric_version}`"),
                )?;
            }
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            let config_file = launcher_config_file(&launcher_root);
            let config = load_launcher_config(&config_file)?;
            write_log(
                stderr,
                format_args!(
                    "Using my mods folder {}",
                    config.configured_mods_folder(&launcher_root).display()
                ),
            )?;
            write_log(
                stderr,
                format_args!("Using editor command `{}`", config.editor_command()),
            )?;
            let created = create_mod(&launcher_root, &config, &create_request)?;
            write_created_mod_output(&created, stdout)?;
            if created.editor_opened {
                write_log(
                    stderr,
                    format_args!("Opened mod project with `{}`", config.editor_command()),
                )?;
            } else {
                write_log(
                    stderr,
                    format_args!(
                        "Editor command `{}` was not available",
                        config.editor_command()
                    ),
                )?;
            }
            write_log(
                stderr,
                format_args!("Finished creating mod project `{}`", created.name),
            )
        }
        Some("build") => {
            let target = args.next().with_context(|| {
                format!(
                    "missing target for `build`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            if target != "mod" {
                bail!(
                    "unknown build target `{target}`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                );
            }

            let build_request = parse_build_mod_request(args)?;

            match build_request.name.as_deref() {
                Some(name) => write_log(stderr, format_args!("Preparing build for mod `{name}`"))?,
                None => write_log(stderr, format_args!("Preparing build for current mod"))?,
            }
            match build_request.version_bump {
                VersionBump::None => {}
                VersionBump::Minor => write_log(stderr, format_args!("Bumping mod minor version"))?,
                VersionBump::Major => write_log(stderr, format_args!("Bumping mod major version"))?,
            }
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            let config_file = launcher_config_file(&launcher_root);
            let config = load_launcher_config(&config_file)?;
            write_log(
                stderr,
                format_args!(
                    "Using my mods folder {}",
                    config.configured_mods_folder(&launcher_root).display()
                ),
            )?;
            let built = build_mod(&launcher_root, &config, cwd, &build_request)?;
            write_built_mod_output(&built, stdout)?;
            write_log(
                stderr,
                format_args!(
                    "Finished building mod `{}` at `{}`",
                    built.name,
                    built.jar.display()
                ),
            )
        }
        Some("clone") => {
            let target = args.next().with_context(|| {
                format!(
                    "missing target for `clone`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            if target != "mod" {
                bail!(
                    "unknown clone target `{target}`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                );
            }

            let git_url = args.next().with_context(|| {
                format!(
                    "missing git URL for `clone mod`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            let clone_request = parse_clone_mod_request(git_url, args)?;

            write_log(
                stderr,
                format_args!(
                    "Preparing clone for mod repository `{}`",
                    clone_request.git_url
                ),
            )?;
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            let config_file = launcher_config_file(&launcher_root);
            let config = load_launcher_config(&config_file)?;
            write_log(
                stderr,
                format_args!(
                    "Using my mods folder {}",
                    config.configured_mods_folder(&launcher_root).display()
                ),
            )?;
            let cloned = clone_mod(&launcher_root, &config, &clone_request)?;
            write_cloned_mod_output(&cloned, stdout)?;
            write_log(
                stderr,
                format_args!(
                    "Finished cloning mod `{}` into `{}`",
                    cloned.name,
                    cloned.path.display()
                ),
            )
        }
        Some("test") => {
            let target = args.next().with_context(|| {
                format!(
                    "missing target for `test`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            if target != "mod" {
                bail!(
                    "unknown test target `{target}`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                );
            }

            let test_request = parse_test_mod_request(args)?;

            match test_request.name.as_deref() {
                Some(name) => write_log(stderr, format_args!("Preparing test for mod `{name}`"))?,
                None => write_log(stderr, format_args!("Preparing test for current mod"))?,
            }
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            write_log(
                stderr,
                format_args!("Ensuring launcher path {}", launcher_root.display()),
            )?;
            ensure_launcher_root(&launcher_root)?;
            let config_file = launcher_config_file(&launcher_root);
            let config = load_launcher_config(&config_file)?;
            write_log(
                stderr,
                format_args!(
                    "Using my mods folder {}",
                    config.configured_mods_folder(&launcher_root).display()
                ),
            )?;
            write_log(
                stderr,
                format_args!("Using offline username `{DEFAULT_TEST_USERNAME}`"),
            )?;
            let tested = test_mod(&launcher_root, &config, cwd, &test_request)?;
            write_tested_mod_output(&tested, stdout)?;
            write_log(
                stderr,
                format_args!(
                    "Finished testing mod `{}` in `{}`",
                    tested.name,
                    tested.launcher_root.display()
                ),
            )
        }
        Some("search") => {
            let target = args.next().with_context(|| {
                format!(
                    "missing target for `search`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            if target != "mod" {
                bail!(
                    "unknown search target `{target}`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                );
            }

            let term = args.next().with_context(|| {
                format!(
                    "missing term for `search mod`\n\n{}",
                    usage_text(DEFAULT_CLI_NAME)
                )
            })?;
            let search_request = parse_search_mod_request(term, args)?;

            write_log(
                stderr,
                format_args!("Searching Modrinth for `{}`", search_request.term),
            )?;
            write_log(stderr, format_args!("Using compiled launcher settings"))?;
            let launcher_root = launcher_root_from_compiled_settings(env_get)?;
            ensure_launcher_root(&launcher_root)?;
            let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
            let mut downloader = HttpDownloader::new();
            let mods = search_modrinth_mods(&versions_folder, &search_request, &mut downloader)?;
            write_searched_mods_output(&mods, stdout)?;
            write_log(stderr, format_args!("Finished searching Modrinth"))
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
    pub requested_version: Option<String>,
    pub connect: Option<String>,
    pub name: Option<String>,
    pub alias: Option<String>,
    pub open: Option<String>,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateModRequest {
    pub name: String,
    pub minecraft_version: Option<String>,
    pub fabric_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildModRequest {
    pub name: Option<String>,
    pub version_bump: VersionBump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionBump {
    None,
    Minor,
    Major,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallModRequest {
    pub name: Option<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneModRequest {
    pub git_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestModRequest {
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchModRequest {
    pub term: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedMod {
    pub name: String,
    pub path: PathBuf,
    pub config: ModProjectConfig,
    pub editor_opened: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltMod {
    pub name: String,
    pub path: PathBuf,
    pub version: String,
    pub jar: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledMod {
    pub name: String,
    pub minecraft_version: String,
    pub alias: String,
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClonedMod {
    pub name: String,
    pub path: PathBuf,
    pub git_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestedMod {
    pub name: String,
    pub minecraft_version: String,
    pub launcher_root: PathBuf,
    pub jar: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchedMod {
    pub title: String,
    pub slug: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModProjectConfig {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<PathBuf>,
    pub mod_id: String,
    pub minecraft_version: String,
    pub fabric_version: String,
    pub mappings: ModMappings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yarn_mappings: Option<String>,
    pub loom_version: String,
    pub gradle_version: String,
    pub java_version: u8,
    pub maven_group: String,
    pub main_class: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModMappings {
    Yarn,
    Unobfuscated,
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

fn parse_run_request(first: String, args: impl Iterator<Item = String>) -> Result<RunRequest> {
    let mut requested_version = None;
    let mut connect = None;
    let mut name = None;
    let mut alias = None;
    let mut open = None;
    let mut username = None;
    let mut args = std::iter::once(first).chain(args).peekable();

    while let Some(arg) = args.next() {
        if arg == "--connect" {
            let value = args
                .next()
                .context("missing value for `--connect` in `run`")?;
            set_run_host(&mut connect, value, "--connect")?;
        } else if let Some(value) = arg.strip_prefix("--connect=") {
            set_run_host(&mut connect, value.to_owned(), "--connect")?;
        } else if arg == "--name" {
            let value = args.next().context("missing value for `--name` in `run`")?;
            set_run_name(&mut name, value)?;
        } else if let Some(value) = arg.strip_prefix("--name=") {
            set_run_name(&mut name, value.to_owned())?;
        } else if arg == "--open" {
            let value = args
                .next_if(|value| !value.starts_with("--"))
                .unwrap_or_else(|| DEFAULT_OPEN_ADDRESS.to_owned());
            set_run_host(&mut open, normalize_socket_address(&value), "--open")?;
        } else if let Some(value) = arg.strip_prefix("--open=") {
            set_run_host(&mut open, normalize_socket_address(value), "--open")?;
        } else if arg == "--alias" {
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
        } else if arg.starts_with("--") {
            bail!("unexpected argument for `run`: `{arg}`");
        } else if requested_version.is_some() {
            bail!("run version was provided more than once");
        } else {
            validate_version_token(&arg, "run version")?;
            requested_version = Some(arg);
        }
    }

    let username = username.context("missing required `--username` for `run`")?;
    if connect.is_some() || name.is_some() {
        if requested_version.is_some() {
            bail!("remote run cannot also provide a local version");
        }
        if open.is_some() {
            bail!("remote mode is incompatible with `--open`");
        }
        if alias.is_some() {
            bail!("remote mode uses `--name`; `--alias` is only for local runs");
        }
        connect
            .as_ref()
            .context("missing required `--connect` for remote `run`")?;
        name.as_ref()
            .context("missing required `--name` for remote `run`")?;
    } else {
        requested_version
            .as_ref()
            .context("missing version for local `run`")?;
    }

    Ok(RunRequest {
        requested_version,
        connect,
        name,
        alias,
        open,
        username,
    })
}

fn parse_create_mod_request(
    name: String,
    mut args: impl Iterator<Item = String>,
) -> Result<CreateModRequest> {
    let mut minecraft_version = None;
    let mut fabric_version = None;

    while let Some(arg) = args.next() {
        if arg == "--version" {
            let value = args
                .next()
                .context("missing value for `--version` in `create mod`")?;
            set_optional_version(&mut minecraft_version, value, "--version")?;
        } else if let Some(value) = arg.strip_prefix("--version=") {
            set_optional_version(&mut minecraft_version, value.to_owned(), "--version")?;
        } else if arg == "--fabric" {
            let value = args
                .next()
                .context("missing value for `--fabric` in `create mod`")?;
            set_optional_version(&mut fabric_version, value, "--fabric")?;
        } else if let Some(value) = arg.strip_prefix("--fabric=") {
            set_optional_version(&mut fabric_version, value.to_owned(), "--fabric")?;
        } else {
            bail!("unexpected argument for `create mod`: `{arg}`");
        }
    }

    validate_path_segment(&name, "mod name")?;

    Ok(CreateModRequest {
        name,
        minecraft_version,
        fabric_version,
    })
}

fn parse_build_mod_request(mut args: impl Iterator<Item = String>) -> Result<BuildModRequest> {
    let mut name = None;
    let mut version_bump = VersionBump::None;

    while let Some(arg) = args.next() {
        if arg == "--minor" {
            set_version_bump(&mut version_bump, VersionBump::Minor, "--minor")?;
        } else if arg == "--major" {
            set_version_bump(&mut version_bump, VersionBump::Major, "--major")?;
        } else if arg.starts_with("--") {
            bail!("unexpected argument for `build mod`: `{arg}`");
        } else {
            set_optional_mod_name(&mut name, arg, "build mod")?;
        }
    }

    Ok(BuildModRequest { name, version_bump })
}

fn parse_install_mod_request(mut args: impl Iterator<Item = String>) -> Result<InstallModRequest> {
    let mut name = None;
    let mut alias = None;

    while let Some(arg) = args.next() {
        if arg == "--alias" {
            let value = args
                .next()
                .context("missing value for `--alias` in `install mod`")?;
            set_install_alias(&mut alias, value)?;
        } else if let Some(value) = arg.strip_prefix("--alias=") {
            set_install_alias(&mut alias, value.to_owned())?;
        } else if arg.starts_with("--") {
            bail!("unexpected argument for `install mod`: `{arg}`");
        } else {
            set_optional_mod_name(&mut name, arg, "install mod")?;
        }
    }

    Ok(InstallModRequest { name, alias })
}

fn parse_clone_mod_request(
    git_url: String,
    mut args: impl Iterator<Item = String>,
) -> Result<CloneModRequest> {
    if let Some(extra) = args.next() {
        bail!("unexpected argument for `clone mod`: `{extra}`");
    }
    validate_git_url(&git_url)?;
    Ok(CloneModRequest { git_url })
}

fn parse_test_mod_request(mut args: impl Iterator<Item = String>) -> Result<TestModRequest> {
    let mut name = None;

    while let Some(arg) = args.next() {
        if arg.starts_with("--") {
            bail!("unexpected argument for `test mod`: `{arg}`");
        }
        set_optional_mod_name(&mut name, arg, "test mod")?;
    }

    Ok(TestModRequest { name })
}

fn parse_search_mod_request(
    term: String,
    mut args: impl Iterator<Item = String>,
) -> Result<SearchModRequest> {
    if term.trim().is_empty() {
        bail!("search term cannot be empty");
    }
    let mut version = None;

    while let Some(arg) = args.next() {
        if arg == "--version" {
            let value = args
                .next()
                .context("missing value for `--version` in `search mod`")?;
            set_optional_version(&mut version, value, "--version")?;
        } else if let Some(value) = arg.strip_prefix("--version=") {
            set_optional_version(&mut version, value.to_owned(), "--version")?;
        } else {
            bail!("unexpected argument for `search mod`: `{arg}`");
        }
    }

    Ok(SearchModRequest { term, version })
}

fn set_version_bump(
    version_bump: &mut VersionBump,
    requested: VersionBump,
    flag: &str,
) -> Result<()> {
    if *version_bump != VersionBump::None {
        bail!("only one of `--minor` or `--major` can be provided");
    }
    *version_bump = requested;
    if requested == VersionBump::None {
        bail!("invalid version bump flag `{flag}`");
    }
    Ok(())
}

fn set_optional_mod_name(slot: &mut Option<String>, value: String, command: &str) -> Result<()> {
    if slot.is_some() {
        bail!("mod name was provided more than once for `{command}`");
    }
    validate_path_segment(&value, "mod name")?;
    *slot = Some(value);
    Ok(())
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

fn set_run_name(name: &mut Option<String>, value: String) -> Result<()> {
    if name.is_some() {
        bail!("`--name` was provided more than once");
    }
    validate_path_segment(&value, "remote run name")?;
    *name = Some(value);
    Ok(())
}

fn set_run_host(slot: &mut Option<String>, value: String, flag: &str) -> Result<()> {
    if slot.is_some() {
        bail!("`{flag}` was provided more than once");
    }
    validate_host_value(&value, flag)?;
    *slot = Some(value);
    Ok(())
}

fn set_optional_version(slot: &mut Option<String>, value: String, flag: &str) -> Result<()> {
    if slot.is_some() {
        bail!("`{flag}` was provided more than once");
    }
    validate_version_token(&value, flag)?;
    *slot = Some(value);
    Ok(())
}

fn validate_git_url(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("git URL cannot be empty");
    }
    if value.trim() != value {
        bail!("git URL cannot contain leading or trailing whitespace");
    }
    if value.chars().any(char::is_whitespace) {
        bail!("git URL cannot contain whitespace");
    }
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
    resolve_requested_fabric_loader_version(downloader, minecraft_version, None)
}

fn resolve_requested_fabric_loader_version(
    downloader: &mut impl Downloader,
    minecraft_version: &str,
    requested_fabric_version: Option<&str>,
) -> Result<String> {
    let versions_url = fabric_loader_versions_for_minecraft_url(minecraft_version);
    let versions = downloader.download_string(&versions_url)?;
    let versions = parse_fabric_loader_versions_for_minecraft_version(&versions)?;
    if let Some(requested_fabric_version) = requested_fabric_version {
        validate_version_token(requested_fabric_version, "Fabric loader version")?;
        if versions
            .iter()
            .any(|version| version.version == requested_fabric_version)
        {
            return Ok(requested_fabric_version.to_owned());
        }
        bail!(
            "Fabric loader `{requested_fabric_version}` is not compatible with Minecraft `{minecraft_version}`"
        );
    }

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

fn fabric_yarn_versions_for_minecraft_url(minecraft_version: &str) -> String {
    format!("{FABRIC_YARN_VERSIONS_URL}/{minecraft_version}?limit=1")
}

fn resolve_mod_mappings(
    downloader: &mut impl Downloader,
    minecraft_version: &str,
) -> Result<(ModMappings, Option<String>)> {
    let versions_url = fabric_yarn_versions_for_minecraft_url(minecraft_version);
    let versions = downloader.download_string(&versions_url)?;
    let versions: Vec<FabricYarnVersion> =
        serde_json::from_str(&versions).context("failed to parse Fabric Yarn versions response")?;
    if let Some(version) = versions.into_iter().next() {
        return Ok((ModMappings::Yarn, Some(version.version)));
    }
    if uses_unobfuscated_minecraft_version(minecraft_version) {
        return Ok((ModMappings::Unobfuscated, None));
    }

    bail!("no Yarn mappings found for Minecraft `{minecraft_version}`");
}

#[derive(Debug, Deserialize)]
struct FabricYarnVersion {
    version: String,
}

fn uses_unobfuscated_minecraft_version(version: &str) -> bool {
    version_major(version) >= 26 || version.ends_with("_unobfuscated")
}

fn fetch_fabric_loom_version(downloader: &mut impl Downloader) -> Result<String> {
    let metadata = downloader.download_string(FABRIC_LOOM_MAVEN_METADATA_URL)?;
    parse_maven_metadata_release(&metadata)
        .context("Fabric Loom Maven metadata did not include a release or latest version")
}

fn parse_maven_metadata_release(metadata: &str) -> Option<String> {
    extract_xml_tag(metadata, "release").or_else(|| extract_xml_tag(metadata, "latest"))
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let value = xml[start..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn fetch_gradle_current_version(downloader: &mut impl Downloader) -> Result<String> {
    let version = downloader.download_string(GRADLE_CURRENT_VERSION_URL)?;
    let version: GradleCurrentVersion = serde_json::from_str(&version)
        .context("failed to parse Gradle current version response")?;
    validate_version_token(&version.version, "Gradle version")?;
    Ok(version.version)
}

#[derive(Debug, Deserialize)]
struct GradleCurrentVersion {
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricMinecraftVersion {
    pub minecraft_version: String,
    pub fabric_version: String,
}

pub fn create_mod_project(
    launcher_root: &Path,
    config: &LauncherConfig,
    request: &CreateModRequest,
) -> Result<CreatedMod> {
    let mut downloader = HttpDownloader::new();
    let mut commands = ProcessExternalCommands;
    create_mod_project_with_services(
        launcher_root,
        config,
        request,
        &mut downloader,
        &mut commands,
    )
}

fn create_mod_project_with_services(
    launcher_root: &Path,
    config: &LauncherConfig,
    request: &CreateModRequest,
    downloader: &mut impl Downloader,
    commands: &mut impl ExternalCommands,
) -> Result<CreatedMod> {
    validate_path_segment(&request.name, "mod name")?;

    let mods_folder = config.configured_mods_folder(launcher_root);
    let mod_dir = mods_folder.join(&request.name);
    if mod_dir
        .try_exists()
        .with_context(|| format!("failed to inspect `{}`", mod_dir.display()))?
    {
        bail!("mod project already exists at `{}`", mod_dir.display());
    }
    fs::create_dir_all(&mods_folder)
        .with_context(|| format!("failed to create `{}`", mods_folder.display()))?;

    let identity = ModIdentity::from_name(&request.name)?;
    let project_config = resolve_mod_project_config(request, &identity, downloader)?;
    fs::create_dir_all(&mod_dir)
        .with_context(|| format!("failed to create `{}`", mod_dir.display()))?;
    write_mod_project_files(&mod_dir, &project_config)?;
    copy_mod_scaffolding(&mod_dir)?;
    commands.git_init(&mod_dir)?;
    let editor_opened = commands.open_editor(config.editor_command(), &mod_dir)?;

    Ok(CreatedMod {
        name: request.name.clone(),
        path: mod_dir,
        config: project_config,
        editor_opened,
    })
}

fn resolve_mod_project_config(
    request: &CreateModRequest,
    identity: &ModIdentity,
    downloader: &mut impl Downloader,
) -> Result<ModProjectConfig> {
    let manifest = downloader.download_string(MINECRAFT_VERSION_MANIFEST_URL)?;
    let manifest = parse_minecraft_version_manifest(&manifest)?;
    let requested_version = request.minecraft_version.as_deref().unwrap_or("latest");
    let selected = resolve_minecraft_version(&manifest, requested_version)?;
    let minecraft_version = selected.id.clone();
    validate_version_token(&minecraft_version, "Minecraft version")?;

    let fabric_version = resolve_requested_fabric_loader_version(
        downloader,
        &minecraft_version,
        request.fabric_version.as_deref(),
    )?;
    let (mappings, yarn_mappings) = resolve_mod_mappings(downloader, &minecraft_version)?;
    let loom_version = fetch_fabric_loom_version(downloader)?;
    let gradle_version = fetch_gradle_current_version(downloader)?;
    let java_version = java_release_for_minecraft_version(&minecraft_version);

    Ok(ModProjectConfig {
        name: request.name.clone(),
        version: DEFAULT_MOD_VERSION.to_owned(),
        build: None,
        mod_id: identity.mod_id.clone(),
        minecraft_version,
        fabric_version,
        mappings,
        yarn_mappings,
        loom_version,
        gradle_version,
        java_version,
        maven_group: identity.maven_group.clone(),
        main_class: identity.main_class.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModIdentity {
    mod_id: String,
    maven_group: String,
    main_class: String,
}

impl ModIdentity {
    fn from_name(name: &str) -> Result<Self> {
        let mod_id = mod_id_from_name(name)?;
        let package_segment = package_segment_from_mod_id(&mod_id);
        let maven_group = format!("com.clearlauncher.{package_segment}");
        let class_name = java_class_name_from_name(name);
        let main_class = format!("{maven_group}.{class_name}");

        Ok(Self {
            mod_id,
            maven_group,
            main_class,
        })
    }
}

fn write_mod_project_files(project_dir: &Path, config: &ModProjectConfig) -> Result<()> {
    write_project_file(
        &project_dir.join("settings.gradle"),
        &settings_gradle_contents(config),
    )?;
    write_project_file(
        &project_dir.join("build.gradle"),
        &build_gradle_contents(config),
    )?;
    write_project_file(
        &project_dir.join("gradle.properties"),
        &gradle_properties_contents(config),
    )?;
    write_project_file(
        &project_dir.join("src/main/resources/fabric.mod.json"),
        &fabric_mod_json_contents(config)?,
    )?;
    write_project_file(
        &project_dir.join("mod.yml"),
        &serde_yaml::to_string(config).context("failed to serialize mod config")?,
    )?;
    write_project_file(&project_dir.join(".gitignore"), gitignore_contents())?;

    let source_path = config.main_class.replace('.', "/");
    write_project_file(
        &project_dir
            .join("src/main/java")
            .join(format!("{source_path}.java")),
        &java_entrypoint_contents(config),
    )
}

fn copy_mod_scaffolding(project_dir: &Path) -> Result<()> {
    let scaffolding_dir = Path::new(SOURCE_FOLDER)
        .join("scaffolding")
        .join(MODS_FOLDER_NAME);
    copy_directory_contents(&scaffolding_dir, project_dir).with_context(|| {
        format!(
            "failed to copy mod scaffolding from `{}` into `{}`",
            scaffolding_dir.display(),
            project_dir.display()
        )
    })
}

fn copy_directory_contents(source_dir: &Path, destination_dir: &Path) -> Result<()> {
    let metadata = fs::metadata(source_dir)
        .with_context(|| format!("failed to inspect `{}`", source_dir.display()))?;
    if !metadata.is_dir() {
        bail!("`{}` is not a directory", source_dir.display());
    }

    fs::create_dir_all(destination_dir)
        .with_context(|| format!("failed to create `{}`", destination_dir.display()))?;
    for entry in fs::read_dir(source_dir)
        .with_context(|| format!("failed to read `{}`", source_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in `{}`", source_dir.display()))?;
        let source = entry.path();
        let destination = destination_dir.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect `{}`", source.display()))?;

        if file_type.is_dir() {
            copy_directory_contents(&source, &destination)?;
        } else if file_type.is_file() {
            copy_file(&source, &destination)?;
        } else if file_type.is_symlink() {
            copy_symlink_target(&source, &destination)?;
        } else {
            bail!(
                "unsupported scaffolding file type at `{}`",
                source.display()
            );
        }
    }

    Ok(())
}

fn copy_symlink_target(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::metadata(source)
        .with_context(|| format!("failed to inspect `{}`", source.display()))?;
    if metadata.is_dir() {
        copy_directory_contents(source, destination)
    } else if metadata.is_file() {
        copy_file(source, destination)
    } else {
        bail!(
            "unsupported scaffolding symlink target at `{}`",
            source.display()
        );
    }
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    fs::copy(source, destination).map(|_| ()).with_context(|| {
        format!(
            "failed to copy `{}` to `{}`",
            source.display(),
            destination.display()
        )
    })
}

fn write_project_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("failed to write `{}`", path.display()))
}

fn settings_gradle_contents(config: &ModProjectConfig) -> String {
    format!(
        r#"pluginManagement {{
    repositories {{
        maven {{
            name = 'Fabric'
            url = 'https://maven.fabricmc.net/'
        }}
        mavenCentral()
        gradlePluginPortal()
    }}
}}

rootProject.name = '{}'
"#,
        config.mod_id
    )
}

fn build_gradle_contents(config: &ModProjectConfig) -> String {
    let java_version = config.java_version;
    let mappings_dependency = match config.mappings {
        ModMappings::Yarn => {
            r#"    mappings "net.fabricmc:yarn:${project.yarn_mappings}:v2"
"#
        }
        ModMappings::Unobfuscated => "",
    };
    let loader_dependency = match config.mappings {
        ModMappings::Yarn => "modImplementation",
        ModMappings::Unobfuscated => "implementation",
    };
    format!(
        r#"plugins {{
    id 'fabric-loom' version "${{loom_version}}"
    id 'maven-publish'
}}

version = project.mod_version
group = project.maven_group

base {{
    archivesName = project.archive_base_name
}}

repositories {{
}}

dependencies {{
    minecraft "com.mojang:minecraft:${{project.minecraft_version}}"
{}    {} "net.fabricmc:fabric-loader:${{project.loader_version}}"
}}

processResources {{
    inputs.property "version", project.version

    filesMatching("fabric.mod.json") {{
        expand "version": project.version
    }}
}}

tasks.withType(JavaCompile).configureEach {{
    it.options.release = {java_version}
}}

java {{
    withSourcesJar()
    sourceCompatibility = JavaVersion.VERSION_{java_version}
    targetCompatibility = JavaVersion.VERSION_{java_version}
}}
"#,
        mappings_dependency, loader_dependency
    )
}

fn gradle_properties_contents(config: &ModProjectConfig) -> String {
    let yarn_mappings = config
        .yarn_mappings
        .as_ref()
        .map(|version| format!("yarn_mappings={version}\n"))
        .unwrap_or_default();
    format!(
        r#"# Gradle
org.gradle.jvmargs=-Xmx1G
org.gradle.parallel=true
gradle_version={}

# Fabric
minecraft_version={}
{}loader_version={}
loom_version={}

# Mod
mod_version={}
maven_group={}
archive_base_name={}
"#,
        config.gradle_version,
        config.minecraft_version,
        yarn_mappings,
        config.fabric_version,
        config.loom_version,
        config.version,
        config.maven_group,
        config.mod_id
    )
}

fn fabric_mod_json_contents(config: &ModProjectConfig) -> Result<String> {
    let manifest = json!({
        "schemaVersion": 1,
        "id": config.mod_id,
        "version": "${version}",
        "name": config.name,
        "description": format!("{} generated by clear-launcher.", config.name),
        "environment": "*",
        "entrypoints": {
            "main": [
                config.main_class
            ]
        },
        "depends": {
            "fabricloader": format!(">={}", config.fabric_version),
            "minecraft": config.minecraft_version,
            "java": format!(">={}", config.java_version)
        }
    });
    serde_json::to_string_pretty(&manifest).context("failed to serialize Fabric mod manifest")
}

fn java_entrypoint_contents(config: &ModProjectConfig) -> String {
    let package = config
        .main_class
        .rsplit_once('.')
        .map(|(package, _)| package)
        .unwrap_or("com.clearlauncher.mod");
    let class_name = config
        .main_class
        .rsplit_once('.')
        .map(|(_, class_name)| class_name)
        .unwrap_or("GeneratedMod");

    format!(
        r#"package {package};

import net.fabricmc.api.ModInitializer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public class {class_name} implements ModInitializer {{
    public static final String MOD_ID = "{}";
    public static final Logger LOGGER = LoggerFactory.getLogger(MOD_ID);

    @Override
    public void onInitialize() {{
        LOGGER.info("Initialized {{}}", MOD_ID);
    }}
}}
"#,
        config.mod_id
    )
}

fn gitignore_contents() -> &'static str {
    ".gradle/\nbuild/\nout/\n*.log\n"
}

fn mod_id_from_name(name: &str) -> Result<String> {
    let mut mod_id = String::new();
    let mut last_was_separator = false;
    for character in name.chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_alphanumeric() || character == '_' {
            mod_id.push(character);
            last_was_separator = false;
        } else if character == '-' || character.is_ascii_whitespace() {
            if !last_was_separator && !mod_id.is_empty() {
                mod_id.push('-');
                last_was_separator = true;
            }
        }
    }

    while mod_id.ends_with('-') {
        mod_id.pop();
    }
    if mod_id.is_empty() {
        bail!("mod name must contain at least one ASCII letter or number");
    }
    if !mod_id
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
    {
        mod_id.insert_str(0, "mod-");
    }
    Ok(mod_id)
}

fn package_segment_from_mod_id(mod_id: &str) -> String {
    let mut segment = mod_id.replace('-', "_");
    if segment
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        segment.insert_str(0, "mod_");
    }
    segment
}

fn java_class_name_from_name(name: &str) -> String {
    let mut class_name = String::new();
    for part in name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
    {
        let mut characters = part.chars();
        if let Some(first) = characters.next() {
            class_name.push(first.to_ascii_uppercase());
            for character in characters {
                class_name.push(character.to_ascii_lowercase());
            }
        }
    }

    if class_name.is_empty() {
        class_name.push_str("GeneratedMod");
    } else if class_name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        class_name.insert_str(0, "Mod");
    }
    class_name
}

fn java_release_for_minecraft_version(version: &str) -> u8 {
    let parts = version_components(version);
    let major = parts.first().and_then(|part| *part).unwrap_or_default();
    let minor = parts.get(1).and_then(|part| *part).unwrap_or_default();
    let patch = parts.get(2).and_then(|part| *part).unwrap_or_default();

    if major >= 26 {
        25
    } else if minor <= 16 {
        8
    } else if minor == 17 {
        16
    } else if minor <= 19 || (minor == 20 && patch <= 4) {
        17
    } else {
        21
    }
}

fn version_major(version: &str) -> u16 {
    version_components(version)
        .first()
        .and_then(|part| *part)
        .unwrap_or_default()
}

fn version_components(version: &str) -> Vec<Option<u16>> {
    version
        .split('-')
        .next()
        .unwrap_or(version)
        .split('.')
        .map(|part| part.parse::<u16>().ok())
        .collect()
}

trait ExternalCommands {
    fn git_init(&mut self, project_dir: &Path) -> Result<()>;
    fn open_editor(&mut self, editor: &str, project_dir: &Path) -> Result<bool>;
}

trait GitCloner {
    fn clone_repo(&mut self, git_url: &str, destination: &Path) -> Result<()>;
}

struct ProcessExternalCommands;

impl ExternalCommands for ProcessExternalCommands {
    fn git_init(&mut self, project_dir: &Path) -> Result<()> {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(project_dir)
            .status()
            .context("failed to start `git init`")?;
        if !status.success() {
            match status.code() {
                Some(code) => bail!("`git init` exited with status code {code}"),
                None => bail!("`git init` was terminated by signal"),
            }
        }
        Ok(())
    }

    fn open_editor(&mut self, editor: &str, project_dir: &Path) -> Result<bool> {
        let mut parts = editor.split_whitespace();
        let Some(program) = parts.next() else {
            return Ok(false);
        };
        match Command::new(program)
            .args(parts)
            .arg(project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("failed to start `{editor}`")),
        }
    }
}

struct ProcessGitCloner;

impl GitCloner for ProcessGitCloner {
    fn clone_repo(&mut self, git_url: &str, destination: &Path) -> Result<()> {
        let status = Command::new("git")
            .arg("clone")
            .arg("--quiet")
            .arg(git_url)
            .arg(destination)
            .status()
            .context("failed to start `git clone`")?;
        if !status.success() {
            match status.code() {
                Some(code) => bail!("`git clone` exited with status code {code}"),
                None => bail!("`git clone` was terminated by signal"),
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedModConfig {
    name: String,
    mod_id: Option<String>,
    minecraft_version: Option<String>,
    fabric_api_version: Option<String>,
    version: Option<String>,
    build: Option<PathBuf>,
    document: YamlValue,
}

pub fn build_mod_project(
    launcher_root: &Path,
    config: &LauncherConfig,
    cwd: &Path,
    request: &BuildModRequest,
) -> Result<BuiltMod> {
    let mut builder = ProcessModBuilder;
    build_mod_project_with_services(launcher_root, config, cwd, request, &mut builder)
}

fn build_mod_project_with_services(
    launcher_root: &Path,
    config: &LauncherConfig,
    cwd: &Path,
    request: &BuildModRequest,
    builder: &mut impl ModBuilder,
) -> Result<BuiltMod> {
    let (mod_dir, mut mod_config) =
        resolve_mod_project_for_request(launcher_root, config, cwd, request.name.as_deref())?;
    let version = next_mod_version(mod_config.version.as_deref(), request.version_bump)?;

    write_gradle_mod_version(&mod_dir, &version)?;
    builder.build(&mod_dir)?;
    let jar = find_mod_build_jar(&mod_dir, &version)?;
    update_loaded_mod_config_build(&mut mod_config, &mod_dir, &version, &jar)?;

    Ok(BuiltMod {
        name: mod_config.name,
        path: mod_dir,
        version,
        jar,
    })
}

pub fn install_mod_project(
    launcher_root: &Path,
    versions_folder: &Path,
    config: &LauncherConfig,
    cwd: &Path,
    request: &InstallModRequest,
) -> Result<InstalledMod> {
    let mut builder = ProcessModBuilder;
    install_mod_project_with_services(
        launcher_root,
        versions_folder,
        config,
        cwd,
        request,
        &mut builder,
    )
}

fn install_mod_project_with_services(
    launcher_root: &Path,
    versions_folder: &Path,
    config: &LauncherConfig,
    cwd: &Path,
    request: &InstallModRequest,
    builder: &mut impl ModBuilder,
) -> Result<InstalledMod> {
    let (mod_dir, mod_config) =
        resolve_mod_project_for_request(launcher_root, config, cwd, request.name.as_deref())?;
    let minecraft_version = mod_config
        .minecraft_version
        .as_deref()
        .context("mod config `minecraft_version` is required to install a mod")?;
    validate_version_token(minecraft_version, "mod config `minecraft_version`")?;
    let alias = request.alias.as_deref().unwrap_or(DEFAULT_INSTALL_ALIAS);
    validate_path_segment(alias, "install alias")?;

    let version_dir = versions_folder.join(minecraft_version).join(alias);
    if !version_dir
        .try_exists()
        .with_context(|| format!("failed to inspect `{}`", version_dir.display()))?
    {
        bail!(
            "Minecraft version `{minecraft_version}` with alias `{alias}` is not installed at `{}`",
            version_dir.display()
        );
    }

    let source = match existing_mod_build_jar(&mod_dir, &mod_config)? {
        Some(jar) => jar,
        None => {
            let built = build_mod_project_with_services(
                launcher_root,
                config,
                cwd,
                &BuildModRequest {
                    name: request.name.clone(),
                    version_bump: VersionBump::None,
                },
                builder,
            )?;
            built.jar
        }
    };

    let mods_dir = ensure_version_mods_dir(&version_dir)?;
    let file_name = source
        .file_name()
        .context("built mod jar path does not include a file name")?;
    let destination = mods_dir.join(file_name);
    let mod_id = match mod_config.mod_id.as_deref() {
        Some(mod_id) => mod_id.to_owned(),
        None => mod_id_from_name(&mod_config.name)?,
    };
    remove_previous_installed_mod_jars(&mods_dir, &mod_id, &destination)?;
    fs::copy(&source, &destination).with_context(|| {
        format!(
            "failed to copy `{}` to `{}`",
            source.display(),
            destination.display()
        )
    })?;
    write_remote_version_manifest(&version_dir, alias, minecraft_version, alias, &mods_dir)?;

    Ok(InstalledMod {
        name: mod_config.name,
        minecraft_version: minecraft_version.to_owned(),
        alias: alias.to_owned(),
        source,
        destination,
    })
}

pub fn clone_mod_project(
    launcher_root: &Path,
    config: &LauncherConfig,
    request: &CloneModRequest,
) -> Result<ClonedMod> {
    let mut cloner = ProcessGitCloner;
    clone_mod_project_with_services(launcher_root, config, request, &mut cloner)
}

fn clone_mod_project_with_services(
    launcher_root: &Path,
    config: &LauncherConfig,
    request: &CloneModRequest,
    cloner: &mut impl GitCloner,
) -> Result<ClonedMod> {
    validate_git_url(&request.git_url)?;

    let mods_folder = config.configured_mods_folder(launcher_root);
    fs::create_dir_all(&mods_folder)
        .with_context(|| format!("failed to create `{}`", mods_folder.display()))?;
    let temp_dir = available_clone_temp_dir(&mods_folder)?;

    let result = (|| -> Result<ClonedMod> {
        cloner.clone_repo(&request.git_url, &temp_dir)?;
        let mod_config = load_mod_config_from_dir(&temp_dir)
            .context("cloned repository does not contain a valid mod.yml")?;
        let target_dir = mods_folder.join(&mod_config.name);
        if target_dir
            .try_exists()
            .with_context(|| format!("failed to inspect `{}`", target_dir.display()))?
        {
            bail!("mod project already exists at `{}`", target_dir.display());
        }

        fs::rename(&temp_dir, &target_dir).with_context(|| {
            format!(
                "failed to move cloned mod from `{}` to `{}`",
                temp_dir.display(),
                target_dir.display()
            )
        })?;

        Ok(ClonedMod {
            name: mod_config.name,
            path: target_dir,
            git_url: request.git_url.clone(),
        })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&temp_dir);
    }

    result
}

pub fn test_mod_project(
    launcher_root: &Path,
    config: &LauncherConfig,
    cwd: &Path,
    request: &TestModRequest,
) -> Result<TestedMod> {
    let mut builder = ProcessModBuilder;
    let mut downloader = HttpDownloader::new();
    let mut launcher = ProcessJavaLauncher;
    test_mod_project_with_services(
        launcher_root,
        config,
        cwd,
        request,
        &mut builder,
        &mut downloader,
        &mut launcher,
    )
}

fn test_mod_project_with_services(
    launcher_root: &Path,
    config: &LauncherConfig,
    cwd: &Path,
    request: &TestModRequest,
    builder: &mut impl ModBuilder,
    downloader: &mut impl Downloader,
    launcher: &mut impl JavaLauncher,
) -> Result<TestedMod> {
    let (mod_dir, mod_config) =
        resolve_mod_project_for_request(launcher_root, config, cwd, request.name.as_deref())?;
    let minecraft_version = mod_config
        .minecraft_version
        .as_deref()
        .context("mod config `minecraft_version` is required to test a mod")?;
    validate_version_token(minecraft_version, "mod config `minecraft_version`")?;

    let jar = match existing_mod_build_jar(&mod_dir, &mod_config)? {
        Some(jar) => jar,
        None => {
            let built = build_mod_project_with_services(
                launcher_root,
                config,
                cwd,
                &BuildModRequest {
                    name: request.name.clone(),
                    version_bump: VersionBump::None,
                },
                builder,
            )?;
            built.jar
        }
    };

    ensure_local_minecraft_gitignored(&mod_dir)?;
    let local_launcher_root = mod_dir.join(TEST_MINECRAFT_FOLDER_NAME);
    let versions_folder = local_launcher_root.join(VERSIONS_FOLDER_NAME);
    let installed = ensure_minecraft_version_with_downloader(
        &local_launcher_root,
        &versions_folder,
        minecraft_version,
        None,
        downloader,
    )?;
    let version_dir = versions_folder
        .join(&installed.id)
        .join(DEFAULT_INSTALL_ALIAS);
    let mods_dir = reset_version_mods_dir(&version_dir)?;
    if let Some(fabric_api_version) = mod_config.fabric_api_version.as_deref() {
        install_fabric_api_mod(&mods_dir, fabric_api_version, downloader)?;
    }
    let file_name = jar
        .file_name()
        .context("built mod jar path does not include a file name")?;
    let destination = mods_dir.join(file_name);
    fs::copy(&jar, &destination).with_context(|| {
        format!(
            "failed to copy `{}` to `{}`",
            jar.display(),
            destination.display()
        )
    })?;

    let run_request = RunRequest {
        requested_version: Some(installed.id.clone()),
        connect: None,
        name: None,
        alias: None,
        open: None,
        username: DEFAULT_TEST_USERNAME.to_owned(),
    };
    let launched = run_minecraft_version_offline_with_services(
        &local_launcher_root,
        &versions_folder,
        &run_request,
        downloader,
        launcher,
    )?;

    Ok(TestedMod {
        name: mod_config.name,
        minecraft_version: launched.id,
        launcher_root: local_launcher_root,
        jar,
        destination,
    })
}

fn ensure_local_minecraft_gitignored(mod_dir: &Path) -> Result<()> {
    let gitignore = mod_dir.join(".gitignore");
    let mut contents = match fs::read_to_string(&gitignore) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read `{}`", gitignore.display()));
        }
    };
    if contents.lines().any(|line| {
        let line = line.trim();
        line == TEST_MINECRAFT_FOLDER_NAME || line == ".minecraft/"
    }) {
        return Ok(());
    }
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(TEST_MINECRAFT_FOLDER_NAME);
    contents.push_str("/\n");
    fs::write(&gitignore, contents)
        .with_context(|| format!("failed to write `{}`", gitignore.display()))
}

fn reset_version_mods_dir(version_dir: &Path) -> Result<PathBuf> {
    let mods_dir = version_dir.join(MODS_FOLDER_NAME);
    match fs::remove_dir_all(&mods_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to remove `{}`", mods_dir.display()));
        }
    }
    fs::create_dir_all(&mods_dir)
        .with_context(|| format!("failed to create `{}`", mods_dir.display()))?;
    Ok(mods_dir)
}

fn install_fabric_api_mod(
    mods_dir: &Path,
    fabric_api_version: &str,
    downloader: &mut impl Downloader,
) -> Result<PathBuf> {
    let artifact_path = maven_artifact_path(&format!(
        "net.fabricmc.fabric-api:fabric-api:{fabric_api_version}"
    ))?;
    let file_name = Path::new(&artifact_path)
        .file_name()
        .context("Fabric API artifact path does not include a file name")?;
    let destination = mods_dir.join(file_name);
    let url = format!("{FABRIC_MAVEN_URL}/{artifact_path}");
    downloader.download_to_path(&url, &destination)?;
    Ok(destination)
}

fn available_clone_temp_dir(mods_folder: &Path) -> Result<PathBuf> {
    let process_id = std::process::id();
    for counter in 0..1000_u16 {
        let candidate = mods_folder.join(format!(".clear-launcher-clone-{process_id}-{counter}"));
        match candidate.try_exists() {
            Ok(false) => return Ok(candidate),
            Ok(true) => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect `{}`", candidate.display()));
            }
        }
    }

    bail!(
        "could not reserve temporary clone folder under `{}`",
        mods_folder.display()
    )
}

fn resolve_mod_project_for_request(
    launcher_root: &Path,
    config: &LauncherConfig,
    cwd: &Path,
    name: Option<&str>,
) -> Result<(PathBuf, LoadedModConfig)> {
    match name {
        Some(name) => {
            validate_path_segment(name, "mod name")?;
            let mod_dir = config.configured_mods_folder(launcher_root).join(name);
            let mod_config = load_mod_config_from_dir(&mod_dir)?;
            if mod_config.name != name {
                bail!(
                    "requested mod `{name}` does not match mod config name `{}`",
                    mod_config.name
                );
            }
            Ok((mod_dir, mod_config))
        }
        None => {
            let mod_config = load_mod_config_from_dir(cwd)?;
            Ok((cwd.to_path_buf(), mod_config))
        }
    }
}

fn load_mod_config_from_dir(mod_dir: &Path) -> Result<LoadedModConfig> {
    let config_file = mod_dir.join(MOD_CONFIG_FILE_NAME);
    if !config_file.is_file() {
        bail!("mod config file not found at `{}`", config_file.display());
    }
    let contents = fs::read_to_string(&config_file)
        .with_context(|| format!("failed to read `{}`", config_file.display()))?;
    if contents.trim().is_empty() {
        bail!("mod config file `{}` is empty", config_file.display());
    }

    let document: YamlValue = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse `{}`", config_file.display()))?;
    let (name, mod_id, minecraft_version, fabric_api_version, version, build) = {
        let mapping = document
            .as_mapping()
            .with_context(|| format!("`{}` must contain a YAML mapping", config_file.display()))?;
        let name = required_yaml_string(mapping, "name", "mod config `name`")?.to_owned();
        let mod_id = optional_yaml_string(mapping, "mod_id", "mod config `mod_id`")?;
        let minecraft_version = optional_yaml_string(
            mapping,
            "minecraft_version",
            "mod config `minecraft_version`",
        )?;
        let fabric_api_version = optional_yaml_string(
            mapping,
            "fabric_api_version",
            "mod config `fabric_api_version`",
        )?;
        let version = optional_yaml_string(mapping, "version", "mod config `version`")?;
        let build =
            optional_yaml_string(mapping, "build", "mod config `build`")?.map(PathBuf::from);
        (
            name,
            mod_id,
            minecraft_version,
            fabric_api_version,
            version,
            build,
        )
    };

    validate_path_segment(&name, "mod name")?;
    if let Some(mod_id) = mod_id.as_deref() {
        validate_path_segment(mod_id, "mod id")?;
    }
    if let Some(minecraft_version) = minecraft_version.as_deref() {
        validate_version_token(minecraft_version, "mod config `minecraft_version`")?;
    }
    if let Some(fabric_api_version) = fabric_api_version.as_deref() {
        validate_version_token(fabric_api_version, "mod config `fabric_api_version`")?;
    }
    if let Some(version) = version.as_deref() {
        validate_version_token(version, "mod config `version`")?;
    }
    if let Some(build) = build.as_ref() {
        if build.as_os_str().is_empty() {
            bail!("mod config `build` cannot be empty");
        }
    }

    Ok(LoadedModConfig {
        name,
        mod_id,
        minecraft_version,
        fabric_api_version,
        version,
        build,
        document,
    })
}

fn required_yaml_string(mapping: &Mapping, key: &str, label: &str) -> Result<String> {
    optional_yaml_string(mapping, key, label)?.with_context(|| format!("{label} is required"))
}

fn optional_yaml_string(mapping: &Mapping, key: &str, label: &str) -> Result<Option<String>> {
    let key = YamlValue::String(key.to_owned());
    let Some(value) = mapping.get(&key) else {
        return Ok(None);
    };
    match value {
        YamlValue::Null => Ok(None),
        YamlValue::String(value) => {
            if value.trim().is_empty() {
                bail!("{label} cannot be empty");
            }
            if value.trim() != value {
                bail!("{label} cannot contain leading or trailing whitespace");
            }
            Ok(Some(value.clone()))
        }
        _ => bail!("{label} must be a string"),
    }
}

fn update_loaded_mod_config_build(
    mod_config: &mut LoadedModConfig,
    mod_dir: &Path,
    version: &str,
    jar: &Path,
) -> Result<()> {
    let config_file = mod_dir.join(MOD_CONFIG_FILE_NAME);
    let relative_jar = jar
        .strip_prefix(mod_dir)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| jar.to_path_buf());
    let mapping = mod_config
        .document
        .as_mapping_mut()
        .with_context(|| format!("`{}` must contain a YAML mapping", config_file.display()))?;
    set_yaml_string(mapping, "version", version);
    set_yaml_string(mapping, "build", &path_to_string(&relative_jar));

    let contents =
        serde_yaml::to_string(&mod_config.document).context("failed to serialize mod config")?;
    fs::write(&config_file, contents)
        .with_context(|| format!("failed to write `{}`", config_file.display()))?;
    mod_config.version = Some(version.to_owned());
    mod_config.build = Some(relative_jar);
    Ok(())
}

fn set_yaml_string(mapping: &mut Mapping, key: &str, value: &str) {
    mapping.insert(
        YamlValue::String(key.to_owned()),
        YamlValue::String(value.to_owned()),
    );
}

fn next_mod_version(current: Option<&str>, bump: VersionBump) -> Result<String> {
    let current = current.unwrap_or(DEFAULT_MOD_VERSION);
    validate_version_token(current, "mod config `version`")?;
    match bump {
        VersionBump::None => Ok(current.to_owned()),
        VersionBump::Minor => {
            let (major, minor, _) = parse_mod_semver(current)?;
            Ok(format!("{major}.{}.0", minor + 1))
        }
        VersionBump::Major => {
            let (major, _, _) = parse_mod_semver(current)?;
            Ok(format!("{}.0.0", major + 1))
        }
    }
}

fn parse_mod_semver(version: &str) -> Result<(u64, u64, u64)> {
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 {
        bail!("mod version `{version}` must use major.minor.patch numeric format");
    }

    let mut parsed = [0_u64; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.chars().all(|character| character.is_ascii_digit()) {
            bail!("mod version `{version}` must use major.minor.patch numeric format");
        }
        parsed[index] = part
            .parse::<u64>()
            .with_context(|| format!("failed to parse mod version `{version}`"))?;
    }

    Ok((parsed[0], parsed[1], parsed[2]))
}

fn write_gradle_mod_version(mod_dir: &Path, version: &str) -> Result<()> {
    let properties_file = mod_dir.join("gradle.properties");
    let contents = match fs::read_to_string(&properties_file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read `{}`", properties_file.display()));
        }
    };

    let mut found = false;
    let mut updated = String::new();
    for line in contents.lines() {
        if line.starts_with("mod_version=") {
            found = true;
            updated.push_str(&format!("mod_version={version}\n"));
        } else {
            updated.push_str(line);
            updated.push('\n');
        }
    }
    if !found {
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&format!("mod_version={version}\n"));
    }

    fs::write(&properties_file, updated)
        .with_context(|| format!("failed to write `{}`", properties_file.display()))
}

trait ModBuilder {
    fn build(&mut self, mod_dir: &Path) -> Result<()>;
}

struct ProcessModBuilder;

impl ModBuilder for ProcessModBuilder {
    fn build(&mut self, mod_dir: &Path) -> Result<()> {
        let (program, args) = gradle_build_command(mod_dir);
        let status = Command::new(&program)
            .args(&args)
            .current_dir(mod_dir)
            .status()
            .with_context(|| format!("failed to start `{program}`"))?;
        if !status.success() {
            match status.code() {
                Some(code) => bail!("`{}` exited with status code {code}", program),
                None => bail!("`{}` was terminated by signal", program),
            }
        }
        Ok(())
    }
}

fn gradle_build_command(mod_dir: &Path) -> (String, Vec<String>) {
    let unix_wrapper = mod_dir.join("gradlew");
    if unix_wrapper.is_file() {
        return (path_to_string(&unix_wrapper), vec!["build".to_owned()]);
    }

    let windows_wrapper = mod_dir.join("gradlew.bat");
    if windows_wrapper.is_file() {
        return (path_to_string(&windows_wrapper), vec!["build".to_owned()]);
    }

    ("gradle".to_owned(), vec!["build".to_owned()])
}

fn existing_mod_build_jar(mod_dir: &Path, mod_config: &LoadedModConfig) -> Result<Option<PathBuf>> {
    let Some(build_path) = mod_config.build.as_ref() else {
        return Ok(None);
    };
    let build_path = if build_path.is_absolute() {
        build_path.clone()
    } else {
        mod_dir.join(build_path)
    };
    if !build_path.is_file() {
        return Ok(None);
    }
    if !path_has_extension(&build_path, "jar") {
        bail!(
            "mod config `build` does not point to a jar file: `{}`",
            build_path.display()
        );
    }
    Ok(Some(build_path))
}

fn find_mod_build_jar(mod_dir: &Path, version: &str) -> Result<PathBuf> {
    let libs_dir = mod_dir.join(BUILD_FOLDER_NAME).join("libs");
    if !libs_dir.is_dir() {
        bail!(
            "Gradle build output folder not found at `{}`",
            libs_dir.display()
        );
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(&libs_dir)
        .with_context(|| format!("failed to read `{}`", libs_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in `{}`", libs_dir.display()))?;
        let path = entry.path();
        if !path.is_file() || !path_has_extension(&path, "jar") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.contains("-sources") || name.contains("-javadoc") {
            continue;
        }
        candidates.push(path);
    }

    candidates.sort();
    if let Some(path) = candidates.iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(version))
    }) {
        return Ok(path.clone());
    }
    candidates.into_iter().next().with_context(|| {
        format!(
            "Gradle build did not produce a mod jar under `{}`",
            libs_dir.display()
        )
    })
}

fn remove_previous_installed_mod_jars(
    mods_dir: &Path,
    mod_id: &str,
    destination: &Path,
) -> Result<()> {
    for entry in fs::read_dir(mods_dir)
        .with_context(|| format!("failed to read `{}`", mods_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in `{}`", mods_dir.display()))?;
        let path = entry.path();
        if path == destination || !path.is_file() || !path_has_extension(&path, "jar") {
            continue;
        }
        if installed_jar_matches_mod_id(&path, mod_id) {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove `{}`", path.display()))?;
        }
    }
    Ok(())
}

fn installed_jar_matches_mod_id(path: &Path, mod_id: &str) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| {
            stem == mod_id
                || stem
                    .strip_prefix(mod_id)
                    .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
        })
}

fn path_has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RemoteVersionManifest {
    name: String,
    version: String,
    fabric: String,
    mods: BTreeMap<String, String>,
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
    if request.connect.is_some() {
        return run_minecraft_version_remote_with_services(
            launcher_root,
            versions_folder,
            request,
            downloader,
            launcher,
        );
    }
    if let Some(alias) = request.alias.as_deref() {
        validate_path_segment(alias, "run alias")?;
    }

    let requested_version = request
        .requested_version
        .as_deref()
        .context("missing version for local run")?;
    let manifest = downloader.download_string(MINECRAFT_VERSION_MANIFEST_URL)?;
    let manifest = parse_minecraft_version_manifest(&manifest)?;
    let selected = resolve_minecraft_version(&manifest, requested_version)?;
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
    let mods_dir = ensure_version_mods_dir(&version_dir)?;
    ensure_installed_profile_loads_fabric_if_needed(
        launcher_root,
        &version_dir,
        &version_id,
        install_name,
        &mods_dir,
        downloader,
    )?;
    write_remote_version_manifest(
        &version_dir,
        install_name,
        &version_id,
        install_name,
        &mods_dir,
    )?;
    if let Some(open) = request.open.as_deref() {
        spawn_remote_version_server(open, &version_dir, install_name)?;
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

fn run_minecraft_version_remote_with_services(
    launcher_root: &Path,
    versions_folder: &Path,
    request: &RunRequest,
    downloader: &mut impl Downloader,
    launcher: &mut impl JavaLauncher,
) -> Result<LaunchedVersion> {
    let host = request
        .connect
        .as_deref()
        .context("missing required `--connect` for remote run")?;
    let name = request
        .name
        .as_deref()
        .context("missing required `--name` for remote run")?;
    validate_host_value(host, "--connect")?;
    validate_path_segment(name, "remote run name")?;
    if request.open.is_some() {
        bail!("remote mode is incompatible with `--open`");
    }

    let base_url = remote_base_url(host);
    let manifest_url = format!("{base_url}/{REMOTE_MANIFEST_FILE_NAME}");
    let manifest: RemoteVersionManifest = serde_yaml::from_str(
        &downloader
            .download_string(&manifest_url)
            .with_context(|| format!("failed to download remote manifest `{manifest_url}`"))?,
    )
    .context("failed to parse remote version manifest")?;
    validate_remote_manifest(&manifest)?;

    let version_dir = versions_folder.join(&manifest.version).join(name);
    fs::create_dir_all(&version_dir)
        .with_context(|| format!("failed to create `{}`", version_dir.display()))?;
    download_remote_file(
        downloader,
        &format!("{base_url}/version.json"),
        &version_dir.join(format!("{name}.json")),
    )?;
    download_remote_file(
        downloader,
        &format!("{base_url}/client.jar"),
        &version_dir.join(format!("{name}.jar")),
    )?;

    let version_json_path = version_dir.join(format!("{name}.json"));
    let version_json = fs::read_to_string(&version_json_path)
        .with_context(|| format!("failed to read `{}`", version_json_path.display()))?;
    let version_data = parse_minecraft_version_data(&version_json)?;
    if let Some(asset_index) = version_data.asset_index.as_ref() {
        install_assets(launcher_root, asset_index, downloader)?;
    }
    install_libraries(
        launcher_root,
        &version_dir,
        version_data.libraries,
        downloader,
    )?;

    let mods_dir = sync_remote_mods(downloader, &base_url, &version_dir, &manifest)?;
    let synced = write_remote_version_manifest(
        &version_dir,
        &manifest.name,
        &manifest.version,
        name,
        &mods_dir,
    )?;
    if synced != manifest {
        bail!("remote version sync did not converge for `{name}`");
    }

    let command = build_launch_command(
        launcher_root,
        &version_dir,
        &manifest.version,
        name,
        request,
    )?;
    launcher.launch(&command)?;

    Ok(LaunchedVersion {
        id: manifest.version,
        alias: Some(name.to_owned()),
        username: request.username.clone(),
    })
}

fn validate_remote_manifest(manifest: &RemoteVersionManifest) -> Result<()> {
    validate_path_segment(&manifest.name, "remote manifest name")?;
    validate_version_token(&manifest.version, "remote manifest version")?;
    validate_version_token(&manifest.fabric, "remote manifest Fabric version")?;
    for mod_name in manifest.mods.keys() {
        validate_path_segment(mod_name, "remote manifest mod name")?;
        if !path_has_extension(Path::new(mod_name), "jar") {
            bail!("remote manifest mod `{mod_name}` is not a jar");
        }
    }
    Ok(())
}

fn write_remote_version_manifest(
    version_dir: &Path,
    name: &str,
    version_id: &str,
    install_name: &str,
    mods_dir: &Path,
) -> Result<RemoteVersionManifest> {
    let manifest = RemoteVersionManifest {
        name: name.to_owned(),
        version: version_id.to_owned(),
        fabric: installed_fabric_loader_version(version_dir, install_name)?,
        mods: installed_mod_hashes(mods_dir)?,
    };
    let contents =
        serde_yaml::to_string(&manifest).context("failed to serialize remote manifest")?;
    fs::write(version_dir.join(REMOTE_MANIFEST_FILE_NAME), contents).with_context(|| {
        format!(
            "failed to write `{}`",
            version_dir.join(REMOTE_MANIFEST_FILE_NAME).display()
        )
    })?;
    Ok(manifest)
}

fn installed_fabric_loader_version(version_dir: &Path, install_name: &str) -> Result<String> {
    let version_json_path = version_dir.join(format!("{install_name}.json"));
    let version_json = fs::read_to_string(&version_json_path)
        .with_context(|| format!("failed to read `{}`", version_json_path.display()))?;
    let version_data = parse_minecraft_version_data(&version_json)?;
    version_data
        .libraries
        .iter()
        .filter_map(|library| library.name.as_deref())
        .find_map(|name| name.strip_prefix("net.fabricmc:fabric-loader:"))
        .map(str::to_owned)
        .with_context(|| {
            format!(
                "installed Minecraft profile `{}` does not include Fabric loader",
                version_json_path.display()
            )
        })
}

fn installed_mod_hashes(mods_dir: &Path) -> Result<BTreeMap<String, String>> {
    let mut mods = BTreeMap::new();
    for entry in fs::read_dir(mods_dir)
        .with_context(|| format!("failed to read `{}`", mods_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in `{}`", mods_dir.display()))?;
        let path = entry.path();
        if !path.is_file() || !path_has_extension(&path, "jar") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("installed mod jar name is not valid UTF-8")?
            .to_owned();
        mods.insert(file_name, file_md5_hex(&path)?);
    }
    Ok(mods)
}

fn search_modrinth_mods(
    versions_folder: &Path,
    request: &SearchModRequest,
    downloader: &mut impl Downloader,
) -> Result<Vec<SearchedMod>> {
    let minecraft_version = resolve_installed_minecraft_version(
        versions_folder,
        request.version.as_deref(),
        downloader,
    )?;
    let facets = format!(r#"[["project_type:mod"],["versions:{minecraft_version}"]]"#);
    let url = format!(
        "{MODRINTH_SEARCH_URL}?query={}&limit=10&facets={}",
        percent_encode_query(&request.term),
        percent_encode_query(&facets)
    );
    let response: ModrinthSearchResponse = serde_json::from_str(
        &downloader
            .download_string(&url)
            .with_context(|| format!("failed to search Modrinth for `{}`", request.term))?,
    )
    .context("failed to parse Modrinth search response")?;

    Ok(response
        .hits
        .into_iter()
        .map(|hit| SearchedMod {
            title: hit.title,
            slug: hit.slug,
            description: hit.description,
        })
        .collect())
}

fn resolve_installed_minecraft_version(
    versions_folder: &Path,
    requested: Option<&str>,
    downloader: &mut impl Downloader,
) -> Result<String> {
    if let Some(version) = requested {
        validate_version_token(version, "search version")?;
        if versions_folder.join(version).is_dir() {
            return Ok(version.to_owned());
        }
        if let Ok(manifest) = downloader
            .download_string(MINECRAFT_VERSION_MANIFEST_URL)
            .and_then(|manifest| parse_minecraft_version_manifest(&manifest))
        {
            if let Ok(version) = resolve_minecraft_version(&manifest, version) {
                return Ok(version.id.clone());
            }
        }
    }

    let alias = requested.unwrap_or(DEFAULT_INSTALL_ALIAS);
    validate_path_segment(alias, "search version")?;

    for entry in fs::read_dir(versions_folder)
        .with_context(|| format!("failed to read `{}`", versions_folder.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read entry in `{}`", versions_folder.display()))?;
        if entry.path().join(alias).is_dir() {
            let version = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("installed Minecraft version is not UTF-8"))?;
            validate_version_token(&version, "installed Minecraft version")?;
            return Ok(version);
        }
    }

    bail!("installed version or alias `{alias}` was not found")
}

fn percent_encode_query(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[derive(Debug, Deserialize)]
struct ModrinthSearchResponse {
    hits: Vec<ModrinthSearchHit>,
}

#[derive(Debug, Deserialize)]
struct ModrinthSearchHit {
    title: String,
    slug: String,
    description: String,
}

// ponytail: MD5 is only a local change detector; use SHA-256 if this becomes a trust boundary.
fn file_md5_hex(path: &Path) -> Result<String> {
    let contents =
        fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let digest = Md5::digest(&contents);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn sync_remote_mods(
    downloader: &mut impl Downloader,
    base_url: &str,
    version_dir: &Path,
    manifest: &RemoteVersionManifest,
) -> Result<PathBuf> {
    let mods_dir = ensure_version_mods_dir(version_dir)?;
    for entry in fs::read_dir(&mods_dir)
        .with_context(|| format!("failed to read `{}`", mods_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in `{}`", mods_dir.display()))?;
        let path = entry.path();
        if path.is_file() && path_has_extension(&path, "jar") {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("installed mod jar name is not valid UTF-8")?;
            if !manifest.mods.contains_key(file_name) {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove `{}`", path.display()))?;
            }
        }
    }

    for (file_name, expected_hash) in &manifest.mods {
        let destination = mods_dir.join(file_name);
        let current_hash = if destination.is_file() {
            Some(file_md5_hex(&destination)?)
        } else {
            None
        };
        if current_hash.as_deref() != Some(expected_hash.as_str()) {
            let _ = fs::remove_file(&destination);
            downloader.download_to_path(&format!("{base_url}/mods/{file_name}"), &destination)?;
        }
        let actual_hash = file_md5_hex(&destination)?;
        if actual_hash != *expected_hash {
            bail!("downloaded remote mod `{file_name}` did not match manifest hash");
        }
    }

    Ok(mods_dir)
}

fn download_remote_file(downloader: &mut impl Downloader, url: &str, path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to remove `{}`", path.display()));
        }
    }
    downloader.download_to_path(url, path)
}

fn remote_base_url(host: &str) -> String {
    let host = host.trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_owned()
    } else {
        format!("http://{}", normalize_socket_address(host))
    }
}

fn normalize_socket_address(value: &str) -> String {
    if value.contains(':') {
        value.to_owned()
    } else {
        format!("{value}:7878")
    }
}

fn spawn_remote_version_server(
    address: &str,
    version_dir: &Path,
    install_name: &str,
) -> Result<()> {
    let listener = TcpListener::bind(normalize_socket_address(address))
        .with_context(|| format!("failed to bind remote server at `{address}`"))?;
    let version_dir = version_dir.to_path_buf();
    let install_name = install_name.to_owned();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = handle_remote_version_request(stream, &version_dir, &install_name);
        }
    });
    Ok(())
}

// ponytail: tiny stdlib HTTP server; add routing/auth only if remote sharing grows.
fn handle_remote_version_request(
    mut stream: TcpStream,
    version_dir: &Path,
    install_name: &str,
) -> Result<()> {
    let mut buffer = [0_u8; 8192];
    let size = stream.read(&mut buffer).context("failed to read request")?;
    let request = String::from_utf8_lossy(&buffer[..size]);
    let Some(line) = request.lines().next() else {
        return write_http_response(&mut stream, "400 Bad Request", "text/plain", b"bad request");
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let raw_path = parts.next().unwrap_or_default();
    if method != "GET" {
        return write_http_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
        );
    }

    let path = raw_path.split('?').next().unwrap_or(raw_path);
    let resource = match remote_resource_path(version_dir, install_name, path)? {
        Some(resource) => resource,
        None => {
            return write_http_response(&mut stream, "404 Not Found", "text/plain", b"not found");
        }
    };
    let body = fs::read(&resource.0)
        .with_context(|| format!("failed to read `{}`", resource.0.display()))?;
    write_http_response(&mut stream, "200 OK", resource.1, &body)
}

fn remote_resource_path(
    version_dir: &Path,
    install_name: &str,
    path: &str,
) -> Result<Option<(PathBuf, &'static str)>> {
    if path == "/" || path == format!("/{REMOTE_MANIFEST_FILE_NAME}") {
        return Ok(Some((
            version_dir.join(REMOTE_MANIFEST_FILE_NAME),
            "application/x-yaml",
        )));
    }
    if path == "/version.json" {
        return Ok(Some((
            version_dir.join(format!("{install_name}.json")),
            "application/json",
        )));
    }
    if path == "/client.jar" {
        return Ok(Some((
            version_dir.join(format!("{install_name}.jar")),
            "application/java-archive",
        )));
    }
    if let Some(file_name) = path.strip_prefix("/mods/") {
        validate_path_segment(file_name, "remote mod request")?;
        return Ok(Some((
            version_dir.join(MODS_FOLDER_NAME).join(file_name),
            "application/java-archive",
        )));
    }
    Ok(None)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .context("failed to write response headers")?;
    stream
        .write_all(body)
        .context("failed to write response body")
}

fn ensure_installed_profile_loads_fabric_if_needed(
    launcher_root: &Path,
    version_dir: &Path,
    version_id: &str,
    install_name: &str,
    mods_dir: &Path,
    downloader: &mut impl Downloader,
) -> Result<()> {
    if !mods_dir_contains_jars(mods_dir)? {
        return Ok(());
    }

    let version_json_path = version_dir.join(format!("{install_name}.json"));
    let version_json = fs::read_to_string(&version_json_path)
        .with_context(|| format!("failed to read `{}`", version_json_path.display()))?;
    let version_data = parse_minecraft_version_data(&version_json)?;
    if version_data_uses_fabric_loader(&version_data) {
        return Ok(());
    }

    let fabric_loader_version = resolve_fabric_loader_version(downloader, version_id)?;
    let fabric_profile_url = fabric_profile_url(version_id, &fabric_loader_version);
    let fabric_profile_json = downloader.download_string(&fabric_profile_url)?;
    let fabric_libraries = fabric_profile_libraries(&fabric_profile_json)?;
    install_libraries(launcher_root, version_dir, fabric_libraries, downloader)?;

    let upgraded_version_json =
        merge_minecraft_and_fabric_version_data(&version_json, &fabric_profile_json)?;
    fs::write(&version_json_path, upgraded_version_json).with_context(|| {
        format!("failed to upgrade installed Minecraft version `{version_id}` with Fabric loader")
    })
}

fn mods_dir_contains_jars(mods_dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(mods_dir)
        .with_context(|| format!("failed to read `{}`", mods_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in `{}`", mods_dir.display()))?;
        let path = entry.path();
        if path.is_file() && path_has_extension(&path, "jar") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn version_data_uses_fabric_loader(version_data: &MinecraftVersionData) -> bool {
    version_data
        .main_class
        .as_deref()
        .is_some_and(|main_class| main_class.starts_with("net.fabricmc.loader."))
}

fn fabric_profile_libraries(fabric_profile: &str) -> Result<Vec<MinecraftLibrary>> {
    let fabric: Value =
        serde_json::from_str(fabric_profile).context("failed to parse Fabric loader profile")?;
    let Some(libraries) = fabric.get("libraries") else {
        return Ok(Vec::new());
    };
    serde_json::from_value(libraries.clone())
        .context("failed to parse Fabric loader profile libraries")
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
    install_minecraft_version_with_downloader_mode(
        launcher_root,
        versions_folder,
        requested_version,
        alias,
        downloader,
        false,
    )
}

fn ensure_minecraft_version_with_downloader(
    launcher_root: &Path,
    versions_folder: &Path,
    requested_version: &str,
    alias: Option<&str>,
    downloader: &mut impl Downloader,
) -> Result<InstalledVersion> {
    install_minecraft_version_with_downloader_mode(
        launcher_root,
        versions_folder,
        requested_version,
        alias,
        downloader,
        true,
    )
}

fn install_minecraft_version_with_downloader_mode(
    launcher_root: &Path,
    versions_folder: &Path,
    requested_version: &str,
    alias: Option<&str>,
    downloader: &mut impl Downloader,
    allow_existing: bool,
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
        if allow_existing {
            return Ok(InstalledVersion {
                id: version_id,
                alias: alias.map(str::to_owned),
            });
        }
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

fn validate_host_value(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.trim() != value {
        bail!("{label} cannot contain leading or trailing whitespace");
    }
    if value.chars().any(char::is_whitespace) {
        bail!("{label} cannot contain whitespace");
    }
    Ok(())
}

fn validate_version_token(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.trim() != value {
        bail!("{label} cannot contain leading or trailing whitespace");
    }
    if value
        .chars()
        .any(|character| character.is_whitespace() || character == '/' || character == '\\')
    {
        bail!("{label} cannot contain whitespace or path separators");
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

fn write_created_mod_output(created: &CreatedMod, stdout: &mut impl Write) -> Result<()> {
    writeln!(
        stdout,
        "Created mod {} at {}",
        created.name,
        created.path.display()
    )
    .context("failed to write create mod output")
}

fn write_built_mod_output(built: &BuiltMod, stdout: &mut impl Write) -> Result<()> {
    writeln!(
        stdout,
        "Built mod {} {} at {}",
        built.name,
        built.version,
        built.jar.display()
    )
    .context("failed to write build mod output")
}

fn write_installed_mod_output(installed: &InstalledMod, stdout: &mut impl Write) -> Result<()> {
    writeln!(
        stdout,
        "Installed mod {} to Minecraft {} as {} at {}",
        installed.name,
        installed.minecraft_version,
        installed.alias,
        installed.destination.display()
    )
    .context("failed to write install mod output")
}

fn write_cloned_mod_output(cloned: &ClonedMod, stdout: &mut impl Write) -> Result<()> {
    writeln!(
        stdout,
        "Cloned mod {} at {}",
        cloned.name,
        cloned.path.display()
    )
    .context("failed to write clone mod output")
}

fn write_tested_mod_output(tested: &TestedMod, stdout: &mut impl Write) -> Result<()> {
    writeln!(
        stdout,
        "Tested mod {} on Minecraft {} at {}",
        tested.name,
        tested.minecraft_version,
        tested.launcher_root.display()
    )
    .context("failed to write test mod output")
}

fn write_searched_mods_output(mods: &[SearchedMod], stdout: &mut impl Write) -> Result<()> {
    for found in mods {
        writeln!(
            stdout,
            "{} ({}) - {}",
            found.title, found.slug, found.description
        )
        .context("failed to write searched mod output")?;
    }
    Ok(())
}

fn write_usage(cli_name: &str, stdout: &mut impl Write) -> Result<()> {
    write!(stdout, "{}", usage_text(cli_name)).context("failed to write usage")
}

fn usage_text(cli_name: &str) -> String {
    format!(
        "Usage: {cli_name} versions\n       {cli_name} install {{version}}|latest [--alias {{alias}}]\n       {cli_name} install mod [name] [--alias {{alias}}]\n       {cli_name} run {{version}} [--alias {{alias}}] [--open [host]] --username {{username}}\n       {cli_name} run --connect {{host}} --name {{name}} --username {{username}}\n       {cli_name} create mod {{name}} [--version {{minecraft-version}}] [--fabric {{fabric-version}}]\n       {cli_name} build mod [name] [--minor] [--major]\n       {cli_name} clone mod {{git-url}}\n       {cli_name} test mod [name]\n       {cli_name} search mod {{term}} [--version {{version-name}}]\n       {cli_name} configure-path\n       {cli_name} unset-path\n"
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
    fn launcher_config_uses_recipe_fields_and_defaults() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let config_file = launcher_root.join(CONFIG_FILE_NAME);
        fs::create_dir_all(&launcher_root).unwrap();
        fs::write(
            &config_file,
            "path: /tmp/clear-launcher\neditor: idea\nmods_folder: my-mods\n",
        )
        .unwrap();

        let config = load_launcher_config(&config_file).unwrap();

        assert_eq!(
            config.path_symlink,
            Some(PathBuf::from("/tmp/clear-launcher"))
        );
        assert_eq!(config.editor_command(), "idea");
        assert_eq!(
            config.configured_mods_folder(&launcher_root),
            launcher_root.join("my-mods")
        );

        let default_config = LauncherConfig::default();
        assert_eq!(default_config.editor_command(), DEFAULT_EDITOR);
        assert_eq!(
            default_config.configured_mods_folder(&launcher_root),
            launcher_root.join(MODS_FOLDER_NAME)
        );

        let saved_config_file = repo.path().join("saved.yml");
        save_launcher_config(
            &saved_config_file,
            &LauncherConfig {
                path_symlink: Some(PathBuf::from("/tmp/clear-launcher")),
                ..LauncherConfig::default()
            },
        )
        .unwrap();
        let saved = fs::read_to_string(saved_config_file).unwrap();
        assert!(saved.contains("path: /tmp/clear-launcher"));
        assert!(!saved.contains("path_symlink"));
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
            |_, _, _| unreachable!("versions command should not create mods"),
            |_, _, _, _| unreachable!("versions command should not build mods"),
            |_, _, _, _, _| unreachable!("versions command should not install mods"),
            |_, _, _| unreachable!("versions command should not clone mods"),
            |_, _, _, _| unreachable!("versions command should not test mods"),
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
    fn create_mod_command_uses_config_and_options() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let mods_folder = repo.path().join("configured-mods");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&launcher_root).unwrap();
        fs::write(
            launcher_root.join(CONFIG_FILE_NAME),
            format!(
                "editor: test-editor\nmods_folder: {}\n",
                mods_folder.display()
            ),
        )
        .unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec![
                "create".to_owned(),
                "mod".to_owned(),
                "Cool Blocks".to_owned(),
                "--version".to_owned(),
                "1.20.4".to_owned(),
                "--fabric=0.16.14".to_owned(),
            ],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("create mod command should not fetch Fabric versions"),
            |_, _, _| unreachable!("create mod command should not install versions"),
            |_, _, _| unreachable!("create mod command should not run versions"),
            |root, config, request| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(config.editor_command(), "test-editor");
                assert_eq!(config.configured_mods_folder(root), mods_folder);
                assert_eq!(
                    request,
                    &CreateModRequest {
                        name: "Cool Blocks".to_owned(),
                        minecraft_version: Some("1.20.4".to_owned()),
                        fabric_version: Some("0.16.14".to_owned()),
                    }
                );
                Ok(CreatedMod {
                    name: request.name.clone(),
                    path: config.configured_mods_folder(root).join(&request.name),
                    config: sample_mod_project_config(),
                    editor_opened: true,
                })
            },
            |_, _, _, _| unreachable!("create mod command should not build mods"),
            |_, _, _, _, _| unreachable!("create mod command should not install mods"),
            |_, _, _| unreachable!("create mod command should not clone mods"),
            |_, _, _, _| unreachable!("create mod command should not test mods"),
            || unreachable!("create mod command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!(
                "Created mod Cool Blocks at {}\n",
                mods_folder.join("Cool Blocks").display()
            )
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Preparing mod project `Cool Blocks`"));
        assert!(logs.contains("Using requested Minecraft version `1.20.4`"));
        assert!(logs.contains("Using requested Fabric loader `0.16.14`"));
        assert!(logs.contains("Opened mod project with `test-editor`"));
    }

    #[test]
    fn build_mod_command_uses_config_and_options() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let mods_folder = repo.path().join("configured-mods");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&launcher_root).unwrap();
        fs::write(
            launcher_root.join(CONFIG_FILE_NAME),
            format!("mods_folder: {}\n", mods_folder.display()),
        )
        .unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec![
                "build".to_owned(),
                "mod".to_owned(),
                "Cool Blocks".to_owned(),
                "--minor".to_owned(),
            ],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("build mod command should not fetch Fabric versions"),
            |_, _, _| unreachable!("build mod command should not install versions"),
            |_, _, _| unreachable!("build mod command should not run versions"),
            |_, _, _| unreachable!("build mod command should not create mods"),
            |root, config, current_dir, request| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(current_dir, cwd.as_path());
                assert_eq!(config.configured_mods_folder(root), mods_folder);
                assert_eq!(
                    request,
                    &BuildModRequest {
                        name: Some("Cool Blocks".to_owned()),
                        version_bump: VersionBump::Minor,
                    }
                );
                Ok(BuiltMod {
                    name: "Cool Blocks".to_owned(),
                    path: mods_folder.join("Cool Blocks"),
                    version: "1.1.0".to_owned(),
                    jar: mods_folder.join("Cool Blocks/build/libs/cool-blocks-1.1.0.jar"),
                })
            },
            |_, _, _, _, _| unreachable!("build mod command should not install mods"),
            |_, _, _| unreachable!("build mod command should not clone mods"),
            |_, _, _, _| unreachable!("build mod command should not test mods"),
            || unreachable!("build mod command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!(
                "Built mod Cool Blocks 1.1.0 at {}\n",
                mods_folder
                    .join("Cool Blocks/build/libs/cool-blocks-1.1.0.jar")
                    .display()
            )
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Preparing build for mod `Cool Blocks`"));
        assert!(logs.contains("Bumping mod minor version"));
    }

    #[test]
    fn install_mod_command_uses_current_mod_and_alias() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("mods").join("cool-blocks");
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
                "mod".to_owned(),
                "--alias".to_owned(),
                "survival".to_owned(),
            ],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("install mod command should not fetch Fabric versions"),
            |_, _, _| unreachable!("install mod command should not install versions"),
            |_, _, _| unreachable!("install mod command should not run versions"),
            |_, _, _| unreachable!("install mod command should not create mods"),
            |_, _, _, _| unreachable!("install mod command should not build mods directly"),
            |root, versions, _config, current_dir, request| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(versions, versions_folder.as_path());
                assert_eq!(current_dir, cwd.as_path());
                assert_eq!(
                    request,
                    &InstallModRequest {
                        name: None,
                        alias: Some("survival".to_owned()),
                    }
                );
                Ok(InstalledMod {
                    name: "cool-blocks".to_owned(),
                    minecraft_version: "1.20.4".to_owned(),
                    alias: "survival".to_owned(),
                    source: cwd.join("build/libs/cool-blocks-1.0.0.jar"),
                    destination: versions_folder.join("1.20.4/survival/mods/cool-blocks-1.0.0.jar"),
                })
            },
            |_, _, _| unreachable!("install mod command should not clone mods"),
            |_, _, _, _| unreachable!("install mod command should not test mods"),
            || unreachable!("install mod command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!(
                "Installed mod cool-blocks to Minecraft 1.20.4 as survival at {}\n",
                versions_folder
                    .join("1.20.4/survival/mods/cool-blocks-1.0.0.jar")
                    .display()
            )
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Preparing install for current mod"));
        assert!(logs.contains("Using install alias `survival`"));
    }

    #[test]
    fn clone_mod_command_uses_config_and_git_url() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let mods_folder = repo.path().join("configured-mods");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&launcher_root).unwrap();
        fs::write(
            launcher_root.join(CONFIG_FILE_NAME),
            format!("mods_folder: {}\n", mods_folder.display()),
        )
        .unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec![
                "clone".to_owned(),
                "mod".to_owned(),
                "https://example.test/repository-name.git".to_owned(),
            ],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("clone mod command should not fetch Fabric versions"),
            |_, _, _| unreachable!("clone mod command should not install versions"),
            |_, _, _| unreachable!("clone mod command should not run versions"),
            |_, _, _| unreachable!("clone mod command should not create mods"),
            |_, _, _, _| unreachable!("clone mod command should not build mods"),
            |_, _, _, _, _| unreachable!("clone mod command should not install mods"),
            |root, config, request| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(config.configured_mods_folder(root), mods_folder);
                assert_eq!(
                    request,
                    &CloneModRequest {
                        git_url: "https://example.test/repository-name.git".to_owned(),
                    }
                );
                Ok(ClonedMod {
                    name: "mod-yml-name".to_owned(),
                    path: mods_folder.join("mod-yml-name"),
                    git_url: request.git_url.clone(),
                })
            },
            |_, _, _, _| unreachable!("clone mod command should not test mods"),
            || unreachable!("clone mod command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!(
                "Cloned mod mod-yml-name at {}\n",
                mods_folder.join("mod-yml-name").display()
            )
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains(
            "Preparing clone for mod repository `https://example.test/repository-name.git`"
        ));
        assert!(logs.contains("Using my mods folder"));
        assert!(logs.contains("Finished cloning mod `mod-yml-name`"));
    }

    #[test]
    fn test_mod_command_uses_config_and_name() {
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("build").join("nested");
        let home = repo.path().join("home");
        let launcher_root = launcher_root_for_home(&home);
        let mods_folder = repo.path().join("configured-mods");
        let local_launcher_root = mods_folder.join("cool-blocks/.minecraft");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&launcher_root).unwrap();
        fs::write(
            launcher_root.join(CONFIG_FILE_NAME),
            format!("mods_folder: {}\n", mods_folder.display()),
        )
        .unwrap();

        let env = test_env_for(&home, None);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        execute_with_services(
            vec![
                "test".to_owned(),
                "mod".to_owned(),
                "cool-blocks".to_owned(),
            ],
            &cwd,
            &env,
            &mut stdout,
            &mut stderr,
            || unreachable!("test mod command should not fetch Fabric versions"),
            |_, _, _| unreachable!("test mod command should not install versions directly"),
            |_, _, _| unreachable!("test mod command should not run versions directly"),
            |_, _, _| unreachable!("test mod command should not create mods"),
            |_, _, _, _| unreachable!("test mod command should not build mods directly"),
            |_, _, _, _, _| unreachable!("test mod command should not install mods"),
            |_, _, _| unreachable!("test mod command should not clone mods"),
            |root, config, current_dir, request| {
                assert_eq!(root, launcher_root.as_path());
                assert_eq!(config.configured_mods_folder(root), mods_folder);
                assert_eq!(current_dir, cwd.as_path());
                assert_eq!(
                    request,
                    &TestModRequest {
                        name: Some("cool-blocks".to_owned()),
                    }
                );
                Ok(TestedMod {
                    name: "cool-blocks".to_owned(),
                    minecraft_version: "1.20.4".to_owned(),
                    launcher_root: local_launcher_root.clone(),
                    jar: mods_folder.join("cool-blocks/build/libs/cool-blocks-1.0.0.jar"),
                    destination: local_launcher_root
                        .join("versions/1.20.4/default/mods/cool-blocks-1.0.0.jar"),
                })
            },
            || unreachable!("test mod command should not inspect the current executable"),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!(
                "Tested mod cool-blocks on Minecraft 1.20.4 at {}\n",
                local_launcher_root.display()
            )
        );
        let logs = String::from_utf8(stderr).unwrap();
        assert!(logs.contains("Preparing test for mod `cool-blocks`"));
        assert!(logs.contains("Using offline username `Player`"));
        assert!(logs.contains("Finished testing mod `cool-blocks`"));
    }

    #[test]
    fn creates_fabric_mod_project_from_metadata() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let request = CreateModRequest {
            name: "Cool Blocks".to_owned(),
            minecraft_version: None,
            fabric_version: None,
        };
        let yarn_versions_url = fabric_yarn_versions_for_minecraft_url("1.21.8");
        let loader_versions_url = fabric_loader_versions_for_minecraft_url("1.21.8");
        let mut downloader = FakeDownloader::new([
            (
                MINECRAFT_VERSION_MANIFEST_URL,
                r#"
{
  "latest": {
    "release": "1.21.8",
    "snapshot": "25w01a"
  },
  "versions": [
    {
      "id": "1.21.8",
      "type": "release",
      "url": "https://example.test/1.21.8.json"
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
      "version": "0.16.13",
      "stable": false
    }
  },
  {
    "loader": {
      "version": "0.16.14",
      "stable": true
    }
  }
]
"#,
            ),
            (
                yarn_versions_url.as_str(),
                r#"
[
  {
    "gameVersion": "1.21.8",
    "version": "1.21.8+build.1"
  }
]
"#,
            ),
            (
                FABRIC_LOOM_MAVEN_METADATA_URL,
                r#"
<metadata>
  <versioning>
    <latest>1.17-SNAPSHOT</latest>
    <release>1.17.11</release>
  </versioning>
</metadata>
"#,
            ),
            (
                GRADLE_CURRENT_VERSION_URL,
                r#"
{
  "version": "9.5.1",
  "current": true
}
"#,
            ),
        ]);
        let mut commands = FakeExternalCommands::default();

        let created = create_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &request,
            &mut downloader,
            &mut commands,
        )
        .unwrap();

        let project_dir = launcher_root.join("mods").join("Cool Blocks");
        assert_eq!(created.path, project_dir);
        assert_eq!(created.config.mod_id, "cool-blocks");
        assert_eq!(created.config.minecraft_version, "1.21.8");
        assert_eq!(created.config.fabric_version, "0.16.14");
        assert_eq!(created.config.mappings, ModMappings::Yarn);
        assert_eq!(
            created.config.yarn_mappings.as_deref(),
            Some("1.21.8+build.1")
        );
        assert_eq!(created.config.loom_version, "1.17.11");
        assert_eq!(created.config.gradle_version, "9.5.1");
        assert_eq!(created.config.java_version, 21);
        assert!(created.editor_opened);
        assert_eq!(commands.git_init_dirs, vec![project_dir.clone()]);
        assert_eq!(
            commands.editor_launches,
            vec![(DEFAULT_EDITOR.to_owned(), project_dir.clone())]
        );

        assert!(project_dir.join("settings.gradle").is_file());
        assert!(project_dir.join("build.gradle").is_file());
        assert!(project_dir.join("gradle.properties").is_file());
        assert!(project_dir.join("mod.yml").is_file());
        assert!(project_dir.join("MOD.md").is_file());
        let compile_skill = project_dir.join(".codex/skills/compile/SKILL.md");
        assert!(compile_skill.is_file());
        assert!(
            fs::read_to_string(compile_skill)
                .unwrap()
                .contains("mc-mods build")
        );
        assert!(
            project_dir
                .join("src/main/java/com/clearlauncher/cool_blocks/CoolBlocks.java")
                .is_file()
        );

        let gradle_properties = fs::read_to_string(project_dir.join("gradle.properties")).unwrap();
        assert!(gradle_properties.contains("minecraft_version=1.21.8"));
        assert!(gradle_properties.contains("yarn_mappings=1.21.8+build.1"));
        assert!(gradle_properties.contains("loader_version=0.16.14"));
        assert!(gradle_properties.contains("loom_version=1.17.11"));
        assert!(gradle_properties.contains("gradle_version=9.5.1"));

        let mod_config = fs::read_to_string(project_dir.join("mod.yml")).unwrap();
        assert!(mod_config.contains("minecraft_version: 1.21.8"));
        assert!(mod_config.contains("version: 1.0.0"));
        assert!(mod_config.contains("mappings: yarn"));
        assert!(mod_config.contains("fabric_version: 0.16.14"));

        let fabric_mod: Value = serde_json::from_str(
            &fs::read_to_string(project_dir.join("src/main/resources/fabric.mod.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fabric_mod.get("id").and_then(Value::as_str),
            Some("cool-blocks")
        );
        assert_eq!(
            fabric_mod
                .pointer("/entrypoints/main/0")
                .and_then(Value::as_str),
            Some("com.clearlauncher.cool_blocks.CoolBlocks")
        );
        assert_eq!(
            fabric_mod
                .pointer("/depends/fabricloader")
                .and_then(Value::as_str),
            Some(">=0.16.14")
        );
    }

    #[test]
    fn creates_unobfuscated_mod_project_without_yarn_mappings() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let request = CreateModRequest {
            name: "Tesr".to_owned(),
            minecraft_version: None,
            fabric_version: None,
        };
        let yarn_versions_url = fabric_yarn_versions_for_minecraft_url("26.1.2");
        let loader_versions_url = fabric_loader_versions_for_minecraft_url("26.1.2");
        let mut downloader = FakeDownloader::new([
            (
                MINECRAFT_VERSION_MANIFEST_URL,
                r#"
{
  "latest": {
    "release": "26.1.2",
    "snapshot": "26.2-snapshot"
  },
  "versions": [
    {
      "id": "26.1.2",
      "type": "release",
      "url": "https://example.test/26.1.2.json"
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
            (yarn_versions_url.as_str(), "[]"),
            (
                FABRIC_LOOM_MAVEN_METADATA_URL,
                r#"
<metadata>
  <versioning>
    <release>1.17.11</release>
  </versioning>
</metadata>
"#,
            ),
            (
                GRADLE_CURRENT_VERSION_URL,
                r#"
{
  "version": "9.5.1"
}
"#,
            ),
        ]);
        let mut commands = FakeExternalCommands::default();

        let created = create_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &request,
            &mut downloader,
            &mut commands,
        )
        .unwrap();

        let project_dir = launcher_root.join("mods").join("Tesr");
        assert_eq!(created.path, project_dir);
        assert_eq!(created.config.minecraft_version, "26.1.2");
        assert_eq!(created.config.fabric_version, "0.19.3");
        assert_eq!(created.config.mappings, ModMappings::Unobfuscated);
        assert_eq!(created.config.yarn_mappings, None);
        assert_eq!(created.config.java_version, 25);
        assert!(project_dir.is_dir());

        let build_gradle = fs::read_to_string(project_dir.join("build.gradle")).unwrap();
        assert!(!build_gradle.contains("net.fabricmc:yarn"));
        assert!(!build_gradle.contains("mappings "));
        assert!(build_gradle.contains("implementation \"net.fabricmc:fabric-loader:"));

        let gradle_properties = fs::read_to_string(project_dir.join("gradle.properties")).unwrap();
        assert!(gradle_properties.contains("minecraft_version=26.1.2"));
        assert!(!gradle_properties.contains("yarn_mappings="));

        let mod_config = fs::read_to_string(project_dir.join("mod.yml")).unwrap();
        assert!(mod_config.contains("mappings: unobfuscated"));
    }

    #[test]
    fn builds_mod_updates_version_build_path_and_gradle_properties() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let mod_dir = launcher_root.join("mods").join("cool-blocks");
        fs::create_dir_all(&mod_dir).unwrap();
        fs::write(
            mod_dir.join(MOD_CONFIG_FILE_NAME),
            "name: cool-blocks\nmod_id: cool-blocks\nminecraft_version: 1.20.4\nversion: 1.2.3\n",
        )
        .unwrap();
        fs::write(
            mod_dir.join("gradle.properties"),
            "minecraft_version=1.20.4\nmod_version=1.2.3\narchive_base_name=cool-blocks\n",
        )
        .unwrap();

        let mut builder = FakeModBuilder::default();
        let built = build_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            repo.path(),
            &BuildModRequest {
                name: Some("cool-blocks".to_owned()),
                version_bump: VersionBump::Minor,
            },
            &mut builder,
        )
        .unwrap();

        assert_eq!(built.name, "cool-blocks");
        assert_eq!(built.version, "1.3.0");
        assert_eq!(builder.build_dirs, vec![mod_dir.clone()]);
        assert_eq!(fs::read(&built.jar).unwrap(), b"fake jar");

        let gradle_properties = fs::read_to_string(mod_dir.join("gradle.properties")).unwrap();
        assert!(gradle_properties.contains("mod_version=1.3.0"));
        assert!(!gradle_properties.contains("mod_version=1.2.3"));

        let mod_config = fs::read_to_string(mod_dir.join(MOD_CONFIG_FILE_NAME)).unwrap();
        assert!(mod_config.contains("version: 1.3.0"));
        assert!(mod_config.contains("build: build/libs/cool-blocks-1.3.0.jar"));
    }

    #[test]
    fn builds_current_mod_defaults_missing_version() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let mod_dir = repo.path().join("standalone-mod");
        fs::create_dir_all(&mod_dir).unwrap();
        fs::write(
            mod_dir.join(MOD_CONFIG_FILE_NAME),
            "name: local-mod\nmod_id: local-mod\nminecraft_version: 1.20.4\n",
        )
        .unwrap();
        fs::write(
            mod_dir.join("gradle.properties"),
            "minecraft_version=1.20.4\narchive_base_name=local-mod\n",
        )
        .unwrap();

        let mut builder = FakeModBuilder::default();
        let built = build_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &mod_dir,
            &BuildModRequest {
                name: None,
                version_bump: VersionBump::None,
            },
            &mut builder,
        )
        .unwrap();

        assert_eq!(built.path, mod_dir);
        assert_eq!(built.version, DEFAULT_MOD_VERSION);
        assert_eq!(builder.build_dirs, vec![built.path.clone()]);
        let mod_config = fs::read_to_string(built.path.join(MOD_CONFIG_FILE_NAME)).unwrap();
        assert!(mod_config.contains("version: 1.0.0"));
        assert!(mod_config.contains("build: build/libs/local-mod-1.0.0.jar"));
    }

    #[test]
    fn installs_mod_building_missing_jar_and_replacing_old_version() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        let version_dir = versions_folder.join("1.20.4").join("survival");
        let installed_mods_dir = version_dir.join(MODS_FOLDER_NAME);
        let mod_dir = launcher_root.join(MODS_FOLDER_NAME).join("cool-blocks");
        fs::create_dir_all(&installed_mods_dir).unwrap();
        fs::create_dir_all(&mod_dir).unwrap();
        fs::write(
            version_dir.join("survival.json"),
            r#"{"downloads":{},"libraries":[{"name":"net.fabricmc:fabric-loader:0.19.3"}]}"#,
        )
        .unwrap();
        fs::write(installed_mods_dir.join("cool-blocks-1.1.0.jar"), "old").unwrap();
        fs::write(installed_mods_dir.join("other-mod-1.0.0.jar"), "other").unwrap();
        fs::write(
            mod_dir.join(MOD_CONFIG_FILE_NAME),
            "name: cool-blocks\nmod_id: cool-blocks\nminecraft_version: 1.20.4\nversion: 1.2.0\n",
        )
        .unwrap();
        fs::write(
            mod_dir.join("gradle.properties"),
            "minecraft_version=1.20.4\nmod_version=1.2.0\narchive_base_name=cool-blocks\n",
        )
        .unwrap();

        let mut builder = FakeModBuilder::default();
        let installed = install_mod_project_with_services(
            &launcher_root,
            &versions_folder,
            &LauncherConfig::default(),
            repo.path(),
            &InstallModRequest {
                name: Some("cool-blocks".to_owned()),
                alias: Some("survival".to_owned()),
            },
            &mut builder,
        )
        .unwrap();

        assert_eq!(installed.name, "cool-blocks");
        assert_eq!(installed.minecraft_version, "1.20.4");
        assert_eq!(installed.alias, "survival");
        assert_eq!(
            installed.destination,
            installed_mods_dir.join("cool-blocks-1.2.0.jar")
        );
        assert_eq!(fs::read(&installed.destination).unwrap(), b"fake jar");
        assert!(!installed_mods_dir.join("cool-blocks-1.1.0.jar").exists());
        assert!(installed_mods_dir.join("other-mod-1.0.0.jar").exists());
        let manifest: RemoteVersionManifest =
            serde_yaml::from_str(&fs::read_to_string(version_dir.join("remote.yml")).unwrap())
                .unwrap();
        assert_eq!(manifest.name, "survival");
        assert_eq!(manifest.version, "1.20.4");
        assert_eq!(manifest.fabric, "0.19.3");
        assert!(manifest.mods.contains_key("cool-blocks-1.2.0.jar"));
        assert_eq!(builder.build_dirs, vec![mod_dir]);
    }

    #[test]
    fn searches_modrinth_for_installed_version_alias() {
        let repo = tempfile::tempdir().unwrap();
        let versions_folder = repo.path().join("versions");
        fs::create_dir_all(versions_folder.join("1.20.4/default")).unwrap();
        let facets = percent_encode_query(r#"[["project_type:mod"],["versions:1.20.4"]]"#);
        let url = format!("{MODRINTH_SEARCH_URL}?query=sodium&limit=10&facets={facets}");
        let mut downloader = FakeDownloader::new([(
            url.as_str(),
            r#"{"hits":[{"title":"Sodium","slug":"sodium","description":"Fast renderer"}]}"#,
        )]);

        let mods = search_modrinth_mods(
            &versions_folder,
            &SearchModRequest {
                term: "sodium".to_owned(),
                version: None,
            },
            &mut downloader,
        )
        .unwrap();

        assert_eq!(
            mods,
            vec![SearchedMod {
                title: "Sodium".to_owned(),
                slug: "sodium".to_owned(),
                description: "Fast renderer".to_owned(),
            }]
        );
    }

    #[test]
    fn searches_modrinth_for_installed_version_folder() {
        let repo = tempfile::tempdir().unwrap();
        let versions_folder = repo.path().join("versions");
        fs::create_dir_all(versions_folder.join("26.1.2/default")).unwrap();
        let facets = percent_encode_query(r#"[["project_type:mod"],["versions:26.1.2"]]"#);
        let url = format!("{MODRINTH_SEARCH_URL}?query=iris&limit=10&facets={facets}");
        let mut downloader = FakeDownloader::new([(
            url.as_str(),
            r#"{"hits":[{"title":"Iris","slug":"iris","description":"Shaders"}]}"#,
        )]);

        let mods = search_modrinth_mods(
            &versions_folder,
            &SearchModRequest {
                term: "iris".to_owned(),
                version: Some("26.1.2".to_owned()),
            },
            &mut downloader,
        )
        .unwrap();

        assert_eq!(mods[0].slug, "iris");
    }

    #[test]
    fn searches_modrinth_for_latest_version_request() {
        let repo = tempfile::tempdir().unwrap();
        let versions_folder = repo.path().join("versions");
        fs::create_dir_all(&versions_folder).unwrap();
        let facets = percent_encode_query(r#"[["project_type:mod"],["versions:1.20.4"]]"#);
        let url = format!("{MODRINTH_SEARCH_URL}?query=iris&limit=10&facets={facets}");
        let mut downloader = FakeDownloader::new([
            (
                MINECRAFT_VERSION_MANIFEST_URL,
                r#"{"latest":{"release":"1.20.4","snapshot":"24w01a"},"versions":[{"id":"1.20.4","type":"release","url":"https://example.test/1.20.4.json"}]}"#,
            ),
            (
                url.as_str(),
                r#"{"hits":[{"title":"Iris","slug":"iris","description":"Shaders"}]}"#,
            ),
        ]);

        let mods = search_modrinth_mods(
            &versions_folder,
            &SearchModRequest {
                term: "iris".to_owned(),
                version: Some("latest".to_owned()),
            },
            &mut downloader,
        )
        .unwrap();

        assert_eq!(mods[0].slug, "iris");
    }

    #[test]
    fn tests_current_mod_in_local_minecraft_with_only_that_mod() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let mod_dir = repo.path().join("standalone-mod");
        let jar = mod_dir.join("build/libs/cool-blocks-1.0.0.jar");
        let loader_versions_url = fabric_loader_versions_for_minecraft_url("1.20.4");
        let fabric_profile_endpoint = fabric_profile_url("1.20.4", "0.19.3");
        fs::create_dir_all(jar.parent().unwrap()).unwrap();
        fs::write(&jar, "mod jar").unwrap();
        fs::write(
            mod_dir.join(MOD_CONFIG_FILE_NAME),
            "name: cool-blocks\nmod_id: cool-blocks\nminecraft_version: 1.20.4\nfabric_api_version: 0.99.0+1.20.4\nversion: 1.0.0\nbuild: build/libs/cool-blocks-1.0.0.jar\n",
        )
        .unwrap();

        let mut builder = FakeModBuilder::default();
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
            (
                "https://example.test/1.20.4.json",
                r#"
{
  "type": "release",
  "mainClass": "net.minecraft.client.main.Main",
  "arguments": {
    "jvm": [
      "-cp",
      "${classpath}"
    ],
    "game": [
      "--username",
      "${auth_player_name}",
      "--gameDir",
      "${game_directory}"
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
    },
    {
      "name": "net.fabricmc:fabric-loader:0.19.3"
    }
  ]
}
"#,
            ),
            (
                fabric_profile_endpoint.as_str(),
                r#"
{
  "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
  "arguments": {
    "jvm": [],
    "game": []
  },
  "libraries": [
    {
      "name": "net.fabricmc:fabric-loader:0.19.3",
      "url": "https://maven.fabricmc.net/"
    }
  ]
}
"#,
            ),
        ]);
        let mut launcher = FakeJavaLauncher::default();

        let tested = test_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &mod_dir,
            &TestModRequest { name: None },
            &mut builder,
            &mut downloader,
            &mut launcher,
        )
        .unwrap();

        let local_launcher_root = mod_dir.join(TEST_MINECRAFT_FOLDER_NAME);
        let version_dir = local_launcher_root.join("versions/1.20.4/default");
        let mods_dir = version_dir.join(MODS_FOLDER_NAME);
        assert_eq!(tested.name, "cool-blocks");
        assert_eq!(tested.launcher_root, local_launcher_root);
        assert_eq!(
            fs::read_to_string(mod_dir.join(".gitignore")).unwrap(),
            ".minecraft/\n"
        );
        assert_eq!(fs::read(&tested.destination).unwrap(), b"mod jar");
        assert_eq!(builder.build_dirs, Vec::<PathBuf>::new());

        fs::write(mods_dir.join("other-mod-1.0.0.jar"), "other").unwrap();
        let tested_again = test_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &mod_dir,
            &TestModRequest { name: None },
            &mut builder,
            &mut downloader,
            &mut launcher,
        )
        .unwrap();

        assert_eq!(tested_again.destination, tested.destination);
        let mut local_mods = fs::read_dir(&mods_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        local_mods.sort();
        assert_eq!(
            local_mods,
            vec![
                mods_dir.join("cool-blocks-1.0.0.jar"),
                mods_dir.join("fabric-api-0.99.0+1.20.4.jar"),
            ]
        );
        let command = launcher.command.unwrap();
        assert_eq!(command.current_dir, version_dir);
        assert!(
            command
                .args
                .contains(&"net.fabricmc.loader.impl.launch.knot.KnotClient".to_owned())
        );
        assert!(command.args.contains(&DEFAULT_TEST_USERNAME.to_owned()));
    }

    #[test]
    fn clones_mod_project_to_name_from_mod_config() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let mods_folder = launcher_root.join(MODS_FOLDER_NAME);
        let request = CloneModRequest {
            git_url: "https://example.test/repository-name.git".to_owned(),
        };
        let mut cloner = FakeGitCloner::with_mod_yml("name: mod-yml-name\n");

        let cloned = clone_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &request,
            &mut cloner,
        )
        .unwrap();

        assert_eq!(
            cloned,
            ClonedMod {
                name: "mod-yml-name".to_owned(),
                path: mods_folder.join("mod-yml-name"),
                git_url: request.git_url.clone(),
            }
        );
        assert_eq!(cloner.clones.len(), 1);
        assert_eq!(cloner.clones[0].0, request.git_url);
        assert_eq!(cloner.clones[0].1.parent(), Some(mods_folder.as_path()));
        assert!(
            cloner.clones[0]
                .1
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".clear-launcher-clone-"))
        );
        assert!(
            mods_folder
                .join("mod-yml-name")
                .join(MOD_CONFIG_FILE_NAME)
                .is_file()
        );
        assert!(mods_folder.join("mod-yml-name").join("README.md").is_file());
        assert!(!mods_folder.join("repository-name").exists());
        assert_eq!(clone_temp_leftovers(&mods_folder), 0);
    }

    #[test]
    fn clone_mod_aborts_when_resolved_target_exists() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let mods_folder = launcher_root.join(MODS_FOLDER_NAME);
        let existing = mods_folder.join("existing-mod");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join("local.txt"), "keep").unwrap();
        let request = CloneModRequest {
            git_url: "https://example.test/other-name.git".to_owned(),
        };
        let mut cloner = FakeGitCloner::with_mod_yml("name: existing-mod\n");

        let error = clone_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &request,
            &mut cloner,
        )
        .unwrap_err();

        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            fs::read_to_string(existing.join("local.txt")).unwrap(),
            "keep"
        );
        assert_eq!(clone_temp_leftovers(&mods_folder), 0);
    }

    #[test]
    fn clone_mod_aborts_when_cloned_mod_config_is_missing() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let mods_folder = launcher_root.join(MODS_FOLDER_NAME);
        let request = CloneModRequest {
            git_url: "https://example.test/missing-config.git".to_owned(),
        };
        let mut cloner = FakeGitCloner::without_mod_yml();

        let error = clone_mod_project_with_services(
            &launcher_root,
            &LauncherConfig::default(),
            &request,
            &mut cloner,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("valid mod.yml"));
        assert!(!mods_folder.join("missing-config").exists());
        assert_eq!(clone_temp_leftovers(&mods_folder), 0);
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
            |_, _, _| unreachable!("install command should not create mods"),
            |_, _, _, _| unreachable!("install command should not build mods"),
            |_, _, _, _, _| unreachable!("install command should not install mods"),
            |_, _, _| unreachable!("install command should not clone mods"),
            |_, _, _, _| unreachable!("install command should not test mods"),
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
            |_, _, _| unreachable!("install command should not create mods"),
            |_, _, _, _| unreachable!("install command should not build mods"),
            |_, _, _, _, _| unreachable!("install command should not install mods"),
            |_, _, _| unreachable!("install command should not clone mods"),
            |_, _, _, _| unreachable!("install command should not test mods"),
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
                        requested_version: Some("1.18".to_owned()),
                        connect: None,
                        name: None,
                        alias: Some("survival".to_owned()),
                        open: None,
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
            |_, _, _| unreachable!("run command should not create mods"),
            |_, _, _, _| unreachable!("run command should not build mods"),
            |_, _, _, _, _| unreachable!("run command should not install mods"),
            |_, _, _| unreachable!("run command should not clone mods"),
            |_, _, _, _| unreachable!("run command should not test mods"),
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
    },
    {
      "name": "net.fabricmc:fabric-loader:0.19.3"
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
            requested_version: Some("latest".to_owned()),
            connect: None,
            name: None,
            alias: None,
            open: None,
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

    #[test]
    fn run_upgrades_legacy_vanilla_profile_with_installed_mods_to_fabric() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        let version_dir = versions_folder.join("1.20.4/default");
        let mods_dir = version_dir.join(MODS_FOLDER_NAME);
        let library_path = launcher_root.join("libraries/org/example/lib/1.0/lib-1.0.jar");
        let loader_versions_url = fabric_loader_versions_for_minecraft_url("1.20.4");
        let fabric_profile_endpoint = fabric_profile_url("1.20.4", "0.19.3");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(library_path.parent().unwrap()).unwrap();
        fs::write(version_dir.join("default.jar"), "client").unwrap();
        fs::write(&library_path, "library").unwrap();
        fs::write(mods_dir.join("day-counter-1.0.0.jar"), "mod").unwrap();
        fs::write(
            version_dir.join("default.json"),
            r#"
{
  "type": "release",
  "mainClass": "net.minecraft.client.main.Main",
  "arguments": {
    "jvm": [
      "-Djava.library.path=${natives_directory}",
      "-cp",
      "${classpath}"
    ],
    "game": [
      "--username",
      "${auth_player_name}",
      "--gameDir",
      "${game_directory}"
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
            (
                fabric_profile_endpoint.as_str(),
                r#"
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
"#,
            ),
        ]);
        let mut launcher = FakeJavaLauncher::default();
        let request = RunRequest {
            requested_version: Some("latest".to_owned()),
            connect: None,
            name: None,
            alias: None,
            open: None,
            username: "Player_1".to_owned(),
        };

        run_minecraft_version_offline_with_services(
            &launcher_root,
            &versions_folder,
            &request,
            &mut downloader,
            &mut launcher,
        )
        .unwrap();

        let installed_data: Value =
            serde_json::from_str(&fs::read_to_string(version_dir.join("default.json")).unwrap())
                .unwrap();
        assert_eq!(
            installed_data.get("mainClass").and_then(Value::as_str),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
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
            fs::read(
                launcher_root
                    .join("libraries/net/fabricmc/fabric-loader/0.19.3/fabric-loader-0.19.3.jar")
            )
            .unwrap(),
            b"https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.19.3/fabric-loader-0.19.3.jar"
        );
        let command = launcher.command.unwrap();
        assert!(
            command
                .args
                .contains(&"net.fabricmc.loader.impl.launch.knot.KnotClient".to_owned())
        );
        let classpath_index = command
            .args
            .iter()
            .position(|argument| argument == "-cp")
            .unwrap()
            + 1;
        assert!(command.args[classpath_index].contains("fabric-loader-0.19.3.jar"));
    }

    #[test]
    fn run_remote_syncs_manifest_and_mods_before_launch() {
        let repo = tempfile::tempdir().unwrap();
        let launcher_root = repo.path().join("launcher");
        let versions_folder = launcher_root.join(VERSIONS_FOLDER_NAME);
        let base_url = "http://server.test:7878";
        let remote_mod = "remote mod";
        let remote_mod_hash = Md5::digest(remote_mod.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let remote_manifest = format!(
            "name: coop\nversion: 1.20.4\nfabric: 0.19.3\nmods:\n  cool.jar: {remote_mod_hash}\n"
        );
        let version_json = r#"
{
  "type": "release",
  "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
  "arguments": {
    "jvm": [
      "-cp",
      "${classpath}"
    ],
    "game": [
      "--username",
      "${auth_player_name}"
    ]
  },
  "downloads": {},
  "libraries": [
    {
      "name": "net.fabricmc:fabric-loader:0.19.3"
    }
  ]
}
"#;
        let manifest_url = format!("{base_url}/{REMOTE_MANIFEST_FILE_NAME}");
        let version_url = format!("{base_url}/version.json");
        let client_url = format!("{base_url}/client.jar");
        let mod_url = format!("{base_url}/mods/cool.jar");
        let mut downloader = FakeDownloader::new([
            (manifest_url.as_str(), remote_manifest.as_str()),
            (version_url.as_str(), version_json),
            (client_url.as_str(), "client"),
            (mod_url.as_str(), remote_mod),
        ]);
        let mut launcher = FakeJavaLauncher::default();
        let request = RunRequest {
            requested_version: None,
            connect: Some("server.test:7878".to_owned()),
            name: Some("local-coop".to_owned()),
            alias: None,
            open: None,
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
                alias: Some("local-coop".to_owned()),
                username: "Player_1".to_owned(),
            }
        );
        let version_dir = versions_folder.join("1.20.4/local-coop");
        assert_eq!(
            fs::read(version_dir.join("mods/cool.jar")).unwrap(),
            b"remote mod"
        );
        let synced_manifest: RemoteVersionManifest =
            serde_yaml::from_str(&fs::read_to_string(version_dir.join("remote.yml")).unwrap())
                .unwrap();
        assert_eq!(synced_manifest.name, "coop");
        assert_eq!(synced_manifest.version, "1.20.4");
        assert_eq!(synced_manifest.fabric, "0.19.3");
        assert_eq!(synced_manifest.mods.get("cool.jar"), Some(&remote_mod_hash));
        let command = launcher.command.unwrap();
        assert_eq!(command.current_dir, version_dir);
        assert!(
            command
                .args
                .contains(&"net.fabricmc.loader.impl.launch.knot.KnotClient".to_owned())
        );
        assert!(command.args.contains(&"Player_1".to_owned()));
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
            |_, _, _| unreachable!("configure-path should not create mods"),
            |_, _, _, _| unreachable!("configure-path should not build mods"),
            |_, _, _, _, _| unreachable!("configure-path should not install mods"),
            |_, _, _| unreachable!("configure-path should not clone mods"),
            |_, _, _, _| unreachable!("configure-path should not test mods"),
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
            |_, _, _| unreachable!("unset-path should not create mods"),
            |_, _, _, _| unreachable!("unset-path should not build mods"),
            |_, _, _, _, _| unreachable!("unset-path should not install mods"),
            |_, _, _| unreachable!("unset-path should not clone mods"),
            |_, _, _, _| unreachable!("unset-path should not test mods"),
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

    fn sample_mod_project_config() -> ModProjectConfig {
        ModProjectConfig {
            name: "Cool Blocks".to_owned(),
            version: DEFAULT_MOD_VERSION.to_owned(),
            build: None,
            mod_id: "cool-blocks".to_owned(),
            minecraft_version: "1.20.4".to_owned(),
            fabric_version: "0.16.14".to_owned(),
            mappings: ModMappings::Yarn,
            yarn_mappings: Some("1.20.4+build.3".to_owned()),
            loom_version: "1.17.11".to_owned(),
            gradle_version: "9.5.1".to_owned(),
            java_version: 17,
            maven_group: "com.clearlauncher.cool_blocks".to_owned(),
            main_class: "com.clearlauncher.cool_blocks.CoolBlocks".to_owned(),
        }
    }

    struct FakeDownloader {
        strings: HashMap<String, String>,
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct FakeModBuilder {
        build_dirs: Vec<PathBuf>,
    }

    impl ModBuilder for FakeModBuilder {
        fn build(&mut self, mod_dir: &Path) -> Result<()> {
            self.build_dirs.push(mod_dir.to_path_buf());
            let mod_config = load_mod_config_from_dir(mod_dir)?;
            let mod_id = match mod_config.mod_id.as_deref() {
                Some(mod_id) => mod_id.to_owned(),
                None => mod_id_from_name(&mod_config.name)?,
            };
            let version = gradle_mod_version(mod_dir)?;
            let jar = mod_dir
                .join(BUILD_FOLDER_NAME)
                .join("libs")
                .join(format!("{mod_id}-{version}.jar"));
            fs::create_dir_all(jar.parent().unwrap()).unwrap();
            fs::write(jar, b"fake jar").unwrap();
            Ok(())
        }
    }

    fn gradle_mod_version(mod_dir: &Path) -> Result<String> {
        let properties = fs::read_to_string(mod_dir.join("gradle.properties"))?;
        properties
            .lines()
            .find_map(|line| line.strip_prefix("mod_version=").map(str::to_owned))
            .context("test gradle.properties did not include mod_version")
    }

    fn clone_temp_leftovers(mods_folder: &Path) -> usize {
        fs::read_dir(mods_folder)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".clear-launcher-clone-"))
            })
            .count()
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FakeExternalCommands {
        git_init_dirs: Vec<PathBuf>,
        editor_launches: Vec<(String, PathBuf)>,
        editor_available: bool,
    }

    impl Default for FakeExternalCommands {
        fn default() -> Self {
            Self {
                git_init_dirs: Vec::new(),
                editor_launches: Vec::new(),
                editor_available: true,
            }
        }
    }

    impl ExternalCommands for FakeExternalCommands {
        fn git_init(&mut self, project_dir: &Path) -> Result<()> {
            self.git_init_dirs.push(project_dir.to_path_buf());
            Ok(())
        }

        fn open_editor(&mut self, editor: &str, project_dir: &Path) -> Result<bool> {
            if self.editor_available {
                self.editor_launches
                    .push((editor.to_owned(), project_dir.to_path_buf()));
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct FakeGitCloner {
        clones: Vec<(String, PathBuf)>,
        mod_yml: Option<String>,
    }

    impl FakeGitCloner {
        fn with_mod_yml(mod_yml: &str) -> Self {
            Self {
                clones: Vec::new(),
                mod_yml: Some(mod_yml.to_owned()),
            }
        }

        fn without_mod_yml() -> Self {
            Self {
                clones: Vec::new(),
                mod_yml: None,
            }
        }
    }

    impl GitCloner for FakeGitCloner {
        fn clone_repo(&mut self, git_url: &str, destination: &Path) -> Result<()> {
            self.clones
                .push((git_url.to_owned(), destination.to_path_buf()));
            fs::create_dir_all(destination)
                .with_context(|| format!("failed to create `{}`", destination.display()))?;
            fs::write(destination.join("README.md"), "cloned").with_context(|| {
                format!(
                    "failed to write clone fixture at `{}`",
                    destination.display()
                )
            })?;
            if let Some(mod_yml) = self.mod_yml.as_deref() {
                fs::write(destination.join(MOD_CONFIG_FILE_NAME), mod_yml).with_context(|| {
                    format!(
                        "failed to write clone fixture `{}`",
                        destination.join(MOD_CONFIG_FILE_NAME).display()
                    )
                })?;
            }
            Ok(())
        }
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
            let contents = self
                .strings
                .get(url)
                .map(String::as_bytes)
                .unwrap_or_else(|| url.as_bytes());
            fs::write(path, contents)
                .with_context(|| format!("failed to write `{}`", path.display()))
        }
    }
}
