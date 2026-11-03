set dotenv-load := false

dev_env := "env XDG_CONFIG_HOME=$PWD/.dev/config XDG_STATE_HOME=$PWD/.dev/state XDG_DATA_HOME=$PWD/.dev/data XDG_RUNTIME_DIR=$PWD/.dev/run RCLONE_CONFIG=$PWD/.dev/rclone.conf SYNCFAST_SYSTEMD=off SYNCFAST_CLI=$PWD/target/debug/syncfast"

check:
    cargo fmt --all --check && cargo clippy --locked --workspace --all-targets -- -D warnings && cargo nextest run --locked --workspace && cargo test --locked --workspace --doc && cargo deny check

test:
    cargo nextest run --workspace

mutants:
    cargo mutants -p syncfast-core

capture-fixtures:
    crates/syncfast-core/tests/capture_fixtures.sh

dev-env:
    mkdir -p .dev/config/syncfast .dev/state .dev/data .dev/run .dev/remote
    printf '[devremote]\ntype = local\n' > .dev/rclone.conf

run-gui-dev: dev-env
    cargo build -p syncfast-cli
    {{dev_env}} cargo run -p syncfast-gui

run-cli-dev *args: dev-env
    {{dev_env}} cargo run -p syncfast-cli -- {{args}}

dev-reset:
    rm -rf .dev

install:
    cargo install --locked --path crates/syncfast-cli --root ~/.local && cargo install --locked --path crates/syncfast-gui --root ~/.local

hooks:
    lefthook install
