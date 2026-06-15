# Clear Launcher

Clear code minecraft launcher

## Glossary

**Compilation Settings File:** `settings.yml` file located on this repository, this is a **BUILD ONLY** file and **MUST NOT** be referenced at runtime, for runtime stuff use the **Config File**
**Build Folder:** Folder where the output source code will be located
**CLI Name:** Nombre con el cual será instalada la CLI
**Versions Folder:**  {Launcher Path}/versions - Folder where minecraft versions are installed
**Config File:** {Launcher Path}/config.yml file that stores the local configuration

## 1. Compilation Settings

Compilation settings are defined in the `settings.yml` file, that must follow the following properties:

| **Property**              | **Description**                                    | **Required** | **Default**                                  |
| ------------------------- | -------------------------------------------------- | ------------ | -------------------------------------------- |
| **launcher_path**         | Path of the .minecraft versions folders on each OS | no           |                                              |
| **launcher_path.linux**   | Launcher path on linux                             | no           | ~/.config/clear-launcher                     |
| **launcher_path.macos**   | Launcher path on macos                             | no           | ~/Library/Application Support/clear-launcher |
| **launcher_path.windows** | Launcher path on windows                           | no           | %APPDATA%/clear-launcher                     |
| **cli_name**              | Name that the CLI will have when compiled          | no           | clear-launcher                               |

## 2. Stack

**CLI:** Rust



## 3. Sources

**Inside a Minecraft Launcher:** ./sources/inside-a-minecraft-launcher.md

## 4. Building

1. Verify that the necessary tools for building are available, ex. `cargo` for rust, if not, abort
2. Verify that the **Compilation Settings File** exists and its valid, if not, abort
3. Read the **Inside a Minecraft Launcher** source
4. Build the features described at the **App** section in the **Build Folder**, ensure no files are written outside it on the compilation process
5. Build the app with `cargo build` or equivalent

## 5. App

Rust Minecraft Launcher CLI that manages minecraft versions and run them in offline mode, it contains the following commands & capabilities.

**Note:** Most commands require the **launcher_path** folder to exist, if it doesnt exist at runtime it must create it automatically

### 1. [Command] Listing versions

**Usage:** `{CLI Name} versions`

List all existing Fabric versions

### 2. [Command] Installing Versions

**Usage:** `{CLI Name} install {version}|latest `[--alias {alias}]

Installs an specific minecraft version, that version can be one of the following:

- **Specific version**
- **Major Version:** ex. 1.18, in that case, it takes the last one
- **Latest:** "latest" - Uses the latest version

The install will download the specified version at the **Versions Folder** under the {Versions Folder}/{version}/{alias | default}
If such version is already installed, abort.

Alias provide ways to have different setups for the same version

### 3. [Command] Path commands

**Usage:** 

- `{CLI Name} configure-path`: Installs the CLI into the user's PATH and stores the created symlink path into the **Config File**
- `{CLI Name} unset-path` : Uninstalls the CLI from the user's PATH and clears the created symlink from the **Config File**



### 4. [Command] Running minecraft

**Usage:** `{CLI Name} run {version} [--alias {alias}] --username {username}`

Runs the minecraft game in **offline** mode with the specified username, the version is resolved in the similar way of **install**

## 6. CLI Style

**Verbosity:** The CLI by default logs everything that it's doing
**Progress Bars:** All actions that can be measured with a progress bar (Quantificable) **MUST** be represented in an animated progress bar in order to let the user know what is the action that is doing and what's the progress on it