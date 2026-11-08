set dotenv-load := false

dev_env := "env XDG_CONFIG_HOME=$PWD/.dev/config XDG_STATE_HOME=$PWD/.dev/state XDG_DATA_HOME=$PWD/.dev/data XDG_RUNTIME_DIR=$PWD/.dev/run RCLONE_CONFIG=$PWD/.dev/rclone.conf SYNCFAST_SYSTEMD=off SYNCFAST_CLI=$PWD/target/debug/syncfast"

check:
    cargo fmt --all --check && cargo clippy --locked --workspace --all-targets -- -D warnings && cargo nextest run --locked --workspace && cargo test --locked --workspace --doc && cargo deny check

test:
    cargo nextest run --workspace

mutants:
    cargo mutants -p syncfast-core

capture-fixtures *scenarios:
    crates/syncfast-core/tests/capture_fixtures.sh {{scenarios}}

capture-restic-fixtures *scenarios:
    crates/syncfast-core/tests/capture_restic_fixtures.sh {{scenarios}}

dev-env:
    mkdir -p .dev/config/syncfast .dev/state .dev/data .dev/run .dev/remote
    printf '[devremote]\ntype = alias\nremote = %s/.dev/remote\n' "$PWD" > .dev/rclone.conf

# The Wayland socket lives in the real XDG_RUNTIME_DIR, which dev mode replaces.
run-gui-dev: dev-env
    cargo build -p syncfast-cli
    wl="${WAYLAND_DISPLAY:-}"; case "$wl" in ''|/*) ;; *) wl="$XDG_RUNTIME_DIR/$wl";; esac; {{dev_env}} ${wl:+WAYLAND_DISPLAY=$wl} cargo run -p syncfast-gui

run-cli-dev *args: dev-env
    {{dev_env}} cargo run -p syncfast-cli -- {{args}}

dev-reset:
    rm -rf .dev

flatpak_id := "io.github.raphael_soares.syncfast"
cargo_generator_rev := "9d86d336a39b9af38b86f114b6ce5820fe260f91"

# Builds the Flatpak from the working tree and installs it for this user.
flatpak:
    nice -n 19 flatpak run org.flatpak.Builder --user --install --install-deps-from=flathub --force-clean --jobs=4 --state-dir=target/flatpak/state --repo=target/flatpak/repo target/flatpak/build packaging/flatpak/{{flatpak_id}}.yml

# Exports the Flatpak as syncfast.flatpak, for `flatpak install --user syncfast.flatpak` on another machine.
flatpak-bundle: flatpak
    flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo target/flatpak/repo syncfast.flatpak {{flatpak_id}}

# Pulls the latest published build.
flatpak-update:
    flatpak update --user io.github.raphael_soares.syncfast

# Regenerates the offline crate sources the Flatpak build needs; run it after Cargo.lock changes.
flatpak-sources:
    python3 -m venv target/flatpak-tools
    target/flatpak-tools/bin/pip install --quiet aiohttp PyYAML tomlkit
    curl -fsSL https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/{{cargo_generator_rev}}/cargo/flatpak-cargo-generator.py -o target/flatpak-tools/flatpak-cargo-generator.py
    target/flatpak-tools/bin/python target/flatpak-tools/flatpak-cargo-generator.py Cargo.lock -o packaging/flatpak/cargo-sources.json

# Pins the restic that ships in the Flatpak, for example `just pin-restic 0.19.1`.
pin-restic version:
    scripts/pin-restic.sh {{version}}

hooks:
    lefthook install
