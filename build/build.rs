use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_CLI_NAME: &str = "clear-launcher";
const DEFAULT_LINUX_LAUNCHER_PATH: &str = "~/.config/clear-launcher";
const DEFAULT_MACOS_LAUNCHER_PATH: &str = "~/Library/Application Support/clear-launcher";
const DEFAULT_WINDOWS_LAUNCHER_PATH: &str = "%APPDATA%/clear-launcher";

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    launcher_path: LauncherPaths,
    cli_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct LauncherPaths {
    linux: Option<String>,
    macos: Option<String>,
    windows: Option<String>,
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let settings_path = manifest_dir
        .parent()
        .expect("build crate must live under the recipe repository")
        .join("settings.yml");

    println!("cargo:rerun-if-changed={}", settings_path.display());

    let contents = fs::read_to_string(&settings_path)
        .unwrap_or_else(|error| panic!("failed to read `{}`: {error}", settings_path.display()));
    let settings: Settings = serde_yaml::from_str(&contents)
        .unwrap_or_else(|error| panic!("failed to parse `{}`: {error}", settings_path.display()));

    emit_compile_value(
        "CLEAR_LAUNCHER_CLI_NAME",
        configured_value(settings.cli_name.as_deref(), DEFAULT_CLI_NAME),
    );
    emit_compile_value(
        "CLEAR_LAUNCHER_LAUNCHER_PATH_LINUX",
        configured_value(
            settings.launcher_path.linux.as_deref(),
            DEFAULT_LINUX_LAUNCHER_PATH,
        ),
    );
    emit_compile_value(
        "CLEAR_LAUNCHER_LAUNCHER_PATH_MACOS",
        configured_value(
            settings.launcher_path.macos.as_deref(),
            DEFAULT_MACOS_LAUNCHER_PATH,
        ),
    );
    emit_compile_value(
        "CLEAR_LAUNCHER_LAUNCHER_PATH_WINDOWS",
        configured_value(
            settings.launcher_path.windows.as_deref(),
            DEFAULT_WINDOWS_LAUNCHER_PATH,
        ),
    );
}

fn configured_value<'a>(value: Option<&'a str>, default: &'a str) -> &'a str {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
}

fn emit_compile_value(key: &str, value: &str) {
    if value.contains('\n') || value.contains('\r') {
        panic!("`{key}` cannot contain line breaks");
    }

    println!("cargo:rustc-env={key}={value}");
}
