# Clear Launcher

Clear code minecraft launcher

## Glossary

**Compilation Settings File:** `settings.yml` file located on this repository, this is a **BUILD ONLY** file and **MUST NOT** be referenced at runtime, for runtime stuff use the **Config File**
**Build Folder:** Folder where the output source code will be located
**CLI Name:** Nombre con el cual será instalada la CLI
**Versions Folder:**  {Launcher Path}/versions - Folder where minecraft versions are installed
**Config File:** {Launcher Path}/config.yml file that stores the local configuration
**Version Folder:** Do not confuse with **Versions Folder**, the **Version Folder** is an specific installed version of minecraft at `{Versions Folder}/{version}/{alias}`
**My Mods Folder:** Folder where downloaded and created mods lives, this is located on the **Config File**'s mods_folder property and defaults to `{Launcher Path}/mods`
**Version Mod Folder:** Folder located at `{Version Folder}/mods`, it stored installed mod for an specific version alias
**Source Folder:** Current folder where this file is located, it is the actual location of the source code, non dependent on pwd
**Version Manifest File:** File located at `{Version Folder}/remote.yml `containing the information about the mods, minecraft version, etc...



## 1. Compilation Settings & Configuration File

Compilation settings are BUILD ONLY settings defined in the `settings.yml` file, that must follow the following properties:

| **Property**              | **Description**                                    | **Required** | **Default**                                  |
| ------------------------- | -------------------------------------------------- | ------------ | -------------------------------------------- |
| **launcher_path**         | Path of the .minecraft versions folders on each OS | no           |                                              |
| **launcher_path.linux**   | Launcher path on linux                             | no           | ~/.config/clear-launcher                     |
| **launcher_path.macos**   | Launcher path on macos                             | no           | ~/Library/Application Support/clear-launcher |
| **launcher_path.windows** | Launcher path on windows                           | no           | %APPDATA%/clear-launcher                     |
| **cli_name**              | Name that the CLI will have when compiled          | no           | clear-launcher                               |

Configuration file is defined at `{Launcher Path}/config.yml` and it is the runtime configuration for the CLI, it has the following properties

| **Property** | **Description**                                    | **Required** | **Default** |
| ------------ | -------------------------------------------------- | ------------ | ----------- |
| **path**     | Path where the symlink of the PATH  file is stored | no           |             |
| **editor**   | Editor command that will open the mods folder      | no           | code        |

## 2. Stack

**CLI:** Rust



## 3. Sources

**Inside a Minecraft Launcher:** ./sources/inside-a-minecraft-launcher.md
**Fabric Documentation:** Read the context from https://docs.fabricmc.net
**Modrinth API Docs:** https://docs.modrinth.com/api/

### Source Caching

In order to optimize the building process, do not read directly the **Modrinth API Docs**, instead:

1. Verify if there is a `.cache/modrinth-openapi.json`  file, if it exists read it directly
2. If the file does not exists, generate it from the documentation

The `modrinth-openapi.json` is the file you will take as source of truth for api documentation



## 4. Building

1. Verify that the necessary tools for building are available, ex. `cargo` for rust, if not, abort
2. Verify that the **Compilation Settings File** exists and its valid, if not, abort
3. Read the **Inside a Minecraft Launcher** source
4. Have **Fabric Documentation** at hand for any docs you need to research
5. Build the features described at the **App** section in the **Build Folder**, ensure no files are written outside it on the compilation process
6. Build the app with `cargo build` or equivalent

## 5. App

Rust Minecraft Launcher CLI that manages minecraft versions and run them in offline mode, it contains the following commands & capabilities.

**Note:** Most commands require the **launcher_path** folder to exist, if it doesnt exist at runtime it must create it automatically

### 1. [Command] Listing versions

**Usage:** `{CLI Name} versions`

List all existing Minecraft versions that has Fabric versions available and displays it in the following format

Minecraft {Minecraft Version} - Fabric {Fabric Version}

### 2. [Command] Installing Versions

**Usage:** `{CLI Name} install {version}|latest `[--alias {alias}]

Installs an specific minecraft Fabric version, that version can be one of the following:

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

**Usage:** `{CLI Name} run {version | --connect {host} --name {name}} [--alias {alias}] [--open [host = 0.0.0.0]] --username {username}`

Runs the minecraft game in **offline** mode with the specified username, the version is resolved in the similar way of **install**.

There are two ways to run the game, in local mode with **version** or remote mode with the `--connect` flag.



#### 4.1 - Manifest File

Before running a game, the system **must** automatically create (or update) a the **Remote Version Manifest File** with the following fields:

```yaml
name: {name}
version: {minecraft-version}
fabric: {fabric-version}
mods:
	{mod-name}: {hash}
```

This must include every single installed mod jar file for the specified version



#### 4.2 - Remote mode



**Note:** Remote mode is incompatible with open mode.

Running the app in remote mode implies downloading the remote minecraft version and its mod into a folder named after the argument at `--name` that is required.  It must download all the metadata and mod jars, not the mod source data.

It must also keep on sync the **Version Manifest File **, so if any change or discrepancy is detected between the local and remote version, it must resolve it until it stays the same.

The name of the manifest file normally dont match the --name arg, and that's ok, this is because the --name is used to name the local version while the remote version can have different name



#### 4.3 - Open Mode

####

**Note:** Open mode is incompatible with remote mode.

When running with the  `--open [host]` flag, the system exposes an http server listening to that specific port, providing all necessary functionalities for other clients to use it with the  `--remote` flag.



### 5. [Command] Creating Mods

**Usage:** `{CLI Name} create mod {name} [--version {minecraft-version}] [--fabric {fabric-version}]`

Creates a new mod project at `{My Mods Folder}/{name}` and opens it with the Config's **editor** command if available, and creates a mod.yml with the **Mod Configuration File** in that folder, then initializes a git repository

The mod **must** follow this default configuration

- **Minecraft Version:** Defaults to latest
- **Fabric Version:** Defaults to the latest compatible with the minecraft version
- **Loom Version:** Dependent of the Fabric Version
- **Gradle Version:** Defaults to latest LTS

After creating the mod, copy the  `{Source Folder}/scaffolding/mods` into the newly created folder



### 6. [Command] Building Mods

**Usage:** `{CLI Name} build mod [name] [--minor] [--major]`

Builds the minecraft mod at `{My Mods Folder}/{name}`, if name is not provided, read it from the current's folder `mod.yml`, if no `mod.yml` exists or it's invalid, abort.

It Builds the mod and generates the jar and updates the mod.yml with the build property pointing to the resulting jar, if name is not provided read the name from the mod.yml in the current folder

If --minor or --major flags are present, it upgrades the config file's version to the next major version and then build,

If no **version** exists in the config file, it defaults to 1.0.0 even if no flags are present



### 7. [Command] Install Mods

**Usage:** `{CLI Name} install mod [name] [--alias {alias = default}]`

Installing mods is the process of copying the jar file from a mod folder located at `{My Mods Folder}/mods/{name}` to a `{Version Folder}/mods`, if name is not provided, read it from the current's folder `mod.yml`, if no `mod.yml` exists or it's invalid, abort.

The process of installing a mod requires building the mod, so if no jar is built in the target mod, it must build it first.

Installing a mod that is already installed on that **Version Folder** automatically replaces any old version of the mod, so it's effectively an update

It must always update the version on the **Version Manifest File**



### 8. [Command] Downloading Mods from Git URL

**Usage:** `{CLI Name} clone mod {git-url}`

 Clones via git url a repository into `{My Mods Folder}/{name}`, if the folder already exists, abort. The name must be resolved from the mod.yml file and not from the repository name.  If the repository does not contains a valid mod.yml, abort.



### 9. [Command] Testing Mods

**Usage:** `{CLI Name} test mod [name]`

Initializes a full local minecraft copy on the ./.minecraft relative to the mod folder (.gitignored), and run the minecraft game with only this specific mod attached, if name is not provided, use the cwd

### 10. [Command] Searching Modrinth Mods

**Usage:** {CLI Name} search mod {term} [--version {version-name}]

Searches mod via a term via **Modrinth API** related to a version, if no version is provided, use the `default` one.  Returns a list of mods.



### 10. [Command] Downloading Modrinth Mods

**Usage:** {CLI Name} download mod {mod-name} [--version {version-name}]

Installs a mod via a term via **Modrinth API** related to a version, if no version is provided, use the `default` one.

It must download the .jar and update the **Version Manifest File**



## 6. CLI Style

**Verbosity:** The CLI by default logs everything that it's doing
**Progress Bars:** All actions that can be measured with a progress bar (Quantifnoicable) **MUST** be represented in an animated progress bar in order to let the user know what is the action that is doing and what's the progress on it



## 7. Mods

Mods are scoped per **version** and belongs to a **Version Folder** at `{Version Folder}/mods`, the **Fabric Loader** must load the mods inside that folder when launching the minecraft version.



### My Mods

My mods are local versions of mods that are stored in the **My Mods Folder** they are not installed in any version of the game, instead they contains folders with source code that can be modified and installed via mod commands



### Mod Config File

The mod config file is a file located at a **Mod Folder**'s mod.yml and it contains the following properties



| **Property** | **Description**                             | **Required** | **Default** |
| ------------ | ------------------------------------------- | ------------ | ----------- |
| **name**     | ID / Name of the mod                        | yes          |             |
| **build**    | Path of the jarfile generated after build   | no           |             |
| **version**  | Current version of the mod, auto increments | no           |             |
