# Basic Tests

Context: read [../clear-launcher.recipe.md](../clear-launcher.recipe.md) before running these checks.

Run these commands from the repository root. The test uses a temporary `HOME` so it does not read or modify the user's real launcher data.

## 0. Setup

Prerequisites:

- `cargo`
- `java`
- network access to Mojang and Fabric metadata/download endpoints
- a graphical desktop session for the final launch check

```sh
cargo build --manifest-path build/Cargo.toml

export TEST_HOME="$(mktemp -d /tmp/clear-launcher-basic-test.XXXXXX)"
export CLI="$PWD/build/target/debug/clear-launcher"
export LAUNCHER_ROOT="$TEST_HOME/.config/clear-launcher"
```

## 1. Versions

Execute the versions command:

```sh
HOME="$TEST_HOME" "$CLI" versions > "$TEST_HOME/versions.out" 2> "$TEST_HOME/versions.err"
```

Verify:

```sh
test -s "$TEST_HOME/versions.out"
awk 'NF && $0 !~ /^Minecraft .+ - Fabric .+$/ { print "invalid versions row: " $0; bad=1 } END { exit bad }' "$TEST_HOME/versions.out"
grep -q "Fetching Fabric-supported Minecraft versions" "$TEST_HOME/versions.err"
```

Expected result: all non-empty output rows follow this format:

```text
Minecraft {Minecraft Version} - Fabric {Fabric Version}
```

## 2. Install

Execute the install command:

```sh
HOME="$TEST_HOME" "$CLI" install latest --alias test > "$TEST_HOME/install.out" 2> "$TEST_HOME/install.err"
INSTALLED_VERSION="$(sed -n 's/^Installed \(.*\) as test$/\1/p' "$TEST_HOME/install.out")"
```

Verify:

```sh
test -n "$INSTALLED_VERSION"

VERSION_DIR="$LAUNCHER_ROOT/versions/$INSTALLED_VERSION/test"
test -d "$VERSION_DIR"
test -f "$VERSION_DIR/test.jar"
test -f "$VERSION_DIR/test.json"

file "$VERSION_DIR/test.jar" | grep -q "Java archive data"
grep -q '"mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient"' "$VERSION_DIR/test.json"
grep -q '"name": "net.fabricmc:fabric-loader:' "$VERSION_DIR/test.json"
find "$LAUNCHER_ROOT/libraries/net/fabricmc" -type f | grep -q 'fabric-loader'
```

Expected result:

- The install is created under `{Launcher Path}/versions/{resolved version}/test`.
- `test.jar` is the Minecraft client jar.
- `test.json` is a merged Minecraft/Fabric launch profile.
- Fabric loader libraries are installed under `{Launcher Path}/libraries`.

## 3. Creating Mods

Disable editor launching for the unattended test:

```sh
mkdir -p "$LAUNCHER_ROOT"
printf 'editor: clear-launcher-editor-that-does-not-exist\n' > "$LAUNCHER_ROOT/config.yml"
```

Execute the create mod command:

```sh
HOME="$TEST_HOME" "$CLI" create mod test-mod --version "$INSTALLED_VERSION" > "$TEST_HOME/create-mod.out" 2> "$TEST_HOME/create-mod.err"
```

Verify:

```sh
MOD_DIR="$LAUNCHER_ROOT/mods/test-mod"
test -d "$MOD_DIR"
test -d "$MOD_DIR/.git"
test -f "$MOD_DIR/mod.yml"
test -f "$MOD_DIR/build.gradle"
test -f "$MOD_DIR/settings.gradle"
test -f "$MOD_DIR/src/main/resources/fabric.mod.json"
grep -q "minecraft_version: $INSTALLED_VERSION" "$MOD_DIR/mod.yml"
grep -q "Editor command" "$TEST_HOME/create-mod.err"
```

Expected result:

- The mod project is created under `{My Mods Folder}/test-mod`, which defaults to `{Launcher Path}/mods/test-mod`.
- `mod.yml`, Gradle files, `fabric.mod.json`, and a Java entrypoint are created.
- A git repository is initialized.
- The missing editor command is logged and does not fail the command.

## 4. Running Minecraft

The `run` command requires an offline username.

Execute:

```sh
HOME="$TEST_HOME" "$CLI" run latest --alias test --username testuser
```

Verify:

- Minecraft starts using the version installed in step 2.
- The log includes Fabric loader startup, for example `Loading Minecraft ... with Fabric Loader ...`.
- The game opens and reaches the main window.

For unattended verification, bound the launch and inspect the startup log:

```sh
timeout 90s env HOME="$TEST_HOME" "$CLI" run latest --alias test --username testuser > "$TEST_HOME/run.out" 2>&1 || test "$?" -eq 124
grep -q "with Fabric Loader" "$TEST_HOME/run.out"
grep -q "Setting user: testuser" "$TEST_HOME/run.out"
```

Exit code `124` is acceptable only for the unattended command above because `timeout` stops a successfully running game after 90 seconds.
