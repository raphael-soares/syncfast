# syncfast: spec da fase 1

## Contexto

O usuário quer pastas locais como fonte primária, sincronizadas nos dois sentidos com o Google Drive, e configurar tudo num app só, sem manter rclone.conf, arquivos de filtro e units systemd espalhados. A máquina roda Arch Linux com Hyprland/Wayland, barra Noctalia e NetworkManager.

O app se chama **syncfast**:

- **Motor:** `rclone bisync`, executado por units systemd `--user` que o próprio app gera.
- **GUI:** Rust + egui + crates do fastframe, no visual da família Spotifast/ZapFast. Composição de telas e comportamento seguem o Apple HIG (macOS).

Roadmap:

- **Esta spec (fase 1):** o app completo, com padrões sãos e poucas opções.
- **Fase 2:** expõe cada comportamento como opção configurável. A fase 1 já resolve tudo por uma struct `Behavior` para que a fase 2 não mexa no motor.
- **Fase 3:** backup periódico.

Estado final da fase 1:

- App instalado como Flatpak (`io.github.raphael_soares.syncfast`, ADR 0006): `syncfast-gui` abre pelo launcher e a CLI roda com `flatpak run --command=syncfast io.github.raphael_soares.syncfast <comando>`.
- Pastas configuradas pela GUI e sincronizando por timer e inotify.
- Revisão obrigatória antes de qualquer resync ou exclusão em massa.
- Bandeja com estado; notificação quando algo exige ação.
- Setup antigo (mount `/mnt/Drive`, sync do Obsidian) removido.

## Decisões fechadas

### Produto

- **Vocabulário:** a UI chama um par de "sync" (`Add Sync…`, `Remove Sync…`, `3 syncs`). "Folder" só aparece quando se refere a uma pasta de verdade (local ou remota). O código usa `pair`.
- **Pares:** N pares `local ↔ remote:caminho`, livres (raiz com raiz ou subpasta). Local é sempre Path1 e remoto sempre Path2. Pares não podem se sobrepor, nem do lado local nem do remoto.
- **Gatilhos:**
  - Local: inotify com debounce de 30 s e teto de 5 min em edição contínua.
  - Remoto: timer 5 min depois do fim da rodada anterior.
  - Manual: `Sync Now`.
- **Conflito:** o mais novo vence (`--conflict-resolve newer`), e o perdedor fica como `arquivo.conflictN` (`--conflict-loser num`).
- **Versões:** o que for apagado ou sobrescrito no local vai para `$XDG_DATA_HOME/syncfast/versions/<id>/` e fica 30 dias. No remoto, quem cobre é a lixeira e as revisões do Drive.
- **Google Docs nativos:** ignorados (`--drive-skip-gdocs`).
- **Exclusões em massa:** `--max-delete 50` (percentual).
- **Revisão manual:** primeira rodada, filtro alterado, erro crítico não recuperável e excesso de exclusões travam o par até o usuário ver o preview e confirmar. O resync usa `--resync-mode newer`.
- **Filtros:** presets `system_junk` e `dev_dirs` (os dois ligados) mais linhas livres. Sem `--exclude-if-present`, porque `git init` numa pasta já sincronizada faria o bisync apagá-la no Drive.
- **Notificações:** só o que exige ação, e só na transição de estado. Sucesso nunca notifica.
- **Credenciais:** o app lista remotes, cria remote Drive com o client_id do usuário, troca credencial e alerta remote que usa o client_id compartilhado do rclone (para de funcionar em 2026).
- **Progresso ao vivo:** barra determinada, arquivos, bytes, velocidade, ETA e arquivo atual.
- **Primeiro uso:** tela de boas-vindas com 3 passos (Account → Folder → Review), mostrada até o primeiro sync ser confirmado.
- **Bandeja-first:** fechar a janela só esconde, e o app fica residente.
- **Idioma:** só inglês, sem i18n. Todo texto de UI, log e notificação citado aqui vai literal.
- **Mock aprovado:** `/home/raphael/.omp/agent/sessions/-tmp/2026-11-03T14-19-52-094Z_01a1a1c7-db9e-72d9-a8ed-8a8eaa7eda4b/local/syncfast-ui-mock.html` é a referência visual das telas Boas-vindas, Janela principal, Review, Add Sync, Conflicts e Settings › Accounts com o menu da bandeja. Na fatia S1, copiar para `docs/design/ui-mock.html` no repositório.

### Técnicas

- **Workspace Cargo com 3 crates em `crates/`:**
  - `syncfast-core` (lib): paths, config, `Behavior`, filtros, bisync, estado, decisão de rodada, units, rclone, rede, notificação. Não depende de egui, eframe, winit, nem de crates fastframe de UI.
  - `syncfast-cli` (bin `syncfast`): `apply`, `run`, `watch`, `preview`, `status`. As units chamam esse binário, dentro do Flatpak: `flatpak run --command=syncfast io.github.raphael_soares.syncfast <comando>`. É também como o usuário o chama no terminal.
  - `syncfast-gui` (bin `syncfast-gui`): app egui/fastframe. Usa o core como biblioteca e nunca chama o binário da CLI, exceto no modo dev (seção Ambiente).
- **Tokio:**
  - O core expõe `async` para processo (`tokio::process`), arquivo e D-Bus (zbus com feature `tokio`).
  - A CLI usa `#[tokio::main(flavor = "current_thread")]`.
  - A GUI tem um runtime multi-thread numa thread de backend, com canais `Command` (UI → backend) e `Event` (backend → UI), e acorda a UI com `fastframe_shell::Waker` (padrão do zapfast `src/backend.rs`).
- **Erros:** `thiserror` no core (um enum por módulo) e `anyhow` só nos `main`.
- **Caminhos:** um resolvedor só, `syncfast_core::paths::Paths::from_env()`, que lê `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME` e `XDG_RUNTIME_DIR` com os fallbacks XDG (`~/.config`, `~/.local/state`, `~/.local/share`, `/run/user/<uid>`). O `fastframe-dirs` não é usado, porque testes e modo dev controlam tudo por variável de ambiente. Dentro do Flatpak, `XDG_*_HOME` apontam para `~/.var/app/<id>`; o resolvedor então usa `HOST_XDG_*` ou os padrões sob `HOME`, e `XDG_RUNTIME_DIR/app/<id>` no lugar de `XDG_RUNTIME_DIR`, para manter config, estado e versions onde estavam (ADR 0006).

### Visual e UX

- **Família fastframe:**
  - Paleta com os 16 tokens do `fastframe-theme` usados pelo Spotifast.
  - Fonte Inter (`fastframe-fonts`), ícones Lucide (`fastframe-icons`).
  - Temas JSON do usuário em `$XDG_CONFIG_HOME/syncfast/themes/`.
  - Verbo `syncfast-gui reload-themes` (no Flatpak, `flatpak run io.github.raphael_soares.syncfast reload-themes`).
  - Template Noctalia próprio.
  - Acento violeta.
- **Estrutura e comportamento seguem o Apple HIG (macOS):**
  - split view com sidebar
  - toolbar no detalhe
  - sheets para tarefas modais (uma por vez, sempre com Cancel)
  - alert só para ação destrutiva irreversível e incomum
  - Settings na própria janela principal, com botão Voltar
  - menu da bandeja sempre com os mesmos itens (desabilita em vez de esconder)
  - progresso determinado quando mensurável
  - erro mostrado perto do problema
  - botões com verbo e em Title Case, com `…` quando pedem mais informação
  - título de janela nunca é o nome do app

### Qualidade, testes, ambiente

- **Portões locais, sem CI remoto (exceto o empacotamento, ADR 0006):**
  - lints rígidos no workspace
  - `cargo-deny`
  - teste de fronteira de crates
  - `lefthook` (pre-commit e commit-msg)
  - `just check`
- **Testes:**
  - unitários
  - integração com rclone real local↔local
  - UI com `egui_kittest` pela árvore de acessibilidade, sem snapshot de pixel
  - mutação com `cargo-mutants` no core, sob demanda
  - Seguir a skill `testes`: comportamento observável, AAA, nome que diz entrada e resultado, mock só na fronteira.
- **Ambiente:**
  - `rust-toolchain.toml`
  - `justfile`
  - modo dev isolado em `./.dev/` com remote rclone local e sem systemd

## Fatos do ambiente (verificados)

- `rclone v1.75.2` em `/usr/bin/rclone`.
- `~/.config/rclone/rclone.conf` tem o remote `gdrive:` (tipo `drive`) com o client_id compartilhado.
- Setup antigo:
  - `~/.config/systemd/user/rclone-gdrive.service` (mount `/mnt/Drive`, ativo)
  - `~/.config/systemd/user/obsidian-sync.{service,timer}` (bisync de `~/Notes`, timer desabilitado, `~/Notes` não existe mais)
  - `~/.config/rclone/{gdrive-exclude.txt,obsidian-filter.txt}`
  - `~/.cache/rclone/bisync/home_raphael_Notes..gdrive_Notes.*`
- O PATH do systemd do usuário não inclui `~/.local/bin`; o do shell inclui. O Flatpak é o único canal de instalação, então as units chamam `flatpak run` (passo 8, ADR 0006).
- `rustup` instalado sem toolchain. Spotifast e zapfast fixam `channel = "1.98.0"`.
- Portais `xdg-desktop-portal`, `-gtk` e `-hyprland` instalados.
- `fs.inotify.max_user_watches = 524288`.
- NetworkManager ativo.
- Dotfiles com GNU stow:
  - `~/.dotfiles/install.sh`, `camada_desktop()`, chama `linkar <dirs a pré-criar> -- hypr noctalia desktop`.
  - O pacote `desktop` já guarda `.local/share/applications/{zapfast,spotifast}.desktop`.
  - O autostart da sessão é `~/.dotfiles/hypr/.config/hypr/autostart.lua` (`hl.exec_cmd(...)`). O Hyprland não executa `~/.config/autostart/*.desktop`.
- O Noctalia tem templates de comunidade para spotifast e zapfast em `~/.local/state/noctalia/community-templates/{spotifast,zapfast}/`:
  - `template.toml` com `input_path_modes`, `output_path`, `post_hook`, `requires_path`
  - `<app>-dark.json` e `<app>-light.json` mapeando cores Material 3 para os 16 tokens
  - Habilitados em `~/.dotfiles/noctalia/.local/state/noctalia/settings.toml`, `[theme.templates] community_ids`.
- `rclone bisync` (doc oficial e `cmd/bisync/operations.go`, `cmd.go`):
  - Saída 0: ok. Saída 1: não crítico; `--max-delete` estourado cai aqui com `too many deletes`. Saída 2: uso. Saída 7: crítico.
  - Com `--resilient`, um crítico "retryable" sai 7 sem renomear listings. Um crítico não recuperável renomeia `*.lst` para `*.lst-err` e loga `Must run --resync to recover`.
  - Mudar o filters-file obriga `--resync` (o MD5 fica em `<filters>.md5`).
  - `--resync` só copia; não pode ser usado em toda rodada.
  - `--backup-dir1` precisa estar fora do Path1.
  - SIGINT faz graceful shutdown (até ~90 s).
  - `--dry-run` pode relatar exclusões que a rodada real não faria (seção Troubleshooting da doc).

## Approach

Os passos estão em ordem de dependência. Cada um termina com `just check` verde. Passos 2 a 9 são do `syncfast-core` e da CLI; 10 a 13 da GUI; 14 são os testes; 15 é a instalação; 16 é a interface para agentes de IA.

### 1. Bootstrap do repositório

Repositório `~/Desktop/Personal/syncfast`, criado com `git init`, licença MIT.

Arquivos na raiz:

- `rust-toolchain.toml`:

  ```toml
  [toolchain]
  channel = "1.98.0"
  components = ["rustfmt", "clippy", "rust-analyzer"]
  ```

- `Cargo.toml` do workspace:
  - `members = ["crates/*"]`, `resolver = "3"`.
  - `[workspace.package]`: `edition = "2024"`, `rust-version = "1.98"`, `license = "MIT"`, `version = "0.1.0"`.
  - `[workspace.dependencies]` com todas as versões. Os crates herdam com `x.workspace = true`.
  - Profiles copiados do spotifast:
    - `[profile.release]`: `lto = "thin"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`
    - `[profile.dev]`: `debug = "line-tables-only"`
    - `[profile.dev.package."*"]`: `opt-level = 2`
  - `[workspace.lints.rust]`: `unsafe_code = "forbid"`, `missing_debug_implementations = "warn"`.
  - `[workspace.lints.clippy]`:
    - `pedantic = { level = "warn", priority = -1 }`
    - em deny: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr`
    - em allow: `module_name_repetitions`
    - No `syncfast-cli`, `src/main.rs` libera `#![allow(clippy::print_stdout, clippy::print_stderr)]`. É o único lugar que imprime.
  - `[patch.crates-io]`: bloco de https://fastframe.dev/getting-started/, com `ecolor`, `eframe`, `egui`, `egui-wgpu`, `egui-winit`, `egui_glow`, `emath`, `epaint`, `epaint_default_fonts`, `egui_kittest` em `git = "https://github.com/crmne/egui", rev = "6ce633ba5f7f64d19ef6d204f9377da560bb546b"` e `winit` em `git = "https://github.com/crmne/winit", rev = "ed7caa9023f10b397f5b6ec8284a840cbd8a6f65"`. Linha para crate não usado sai.
- `clippy.toml`: `allow-unwrap-in-tests = true`, `allow-expect-in-tests = true`, `allow-panic-in-tests = true`.
- `rustfmt.toml` vazio.
- `deny.toml`:
  - `[licenses] allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0", "OFL-1.1", "MPL-2.0", "CC0-1.0"]`
  - `[advisories]` com vulnerabilidades negadas
  - `[bans] multiple-versions = "warn"`
  - `[sources] unknown-git = "deny"`, `allow-git = ["https://github.com/crmne/fastframe", "https://github.com/crmne/egui", "https://github.com/crmne/winit"]`
- `lefthook.yml`:
  - `pre-commit`: `cargo fmt --all --check` e `cargo clippy --locked --workspace --all-targets -- -D warnings`.
  - `commit-msg`: script que valida a primeira linha contra `^(feat|fix|refactor|perf|test|docs|build|chore)(\([a-z0-9-]+\))?!?: .+` e rejeita `Co-Authored-By`.
- `justfile`, com `set dotenv-load := false`:
  - `check`: `cargo fmt --all --check && cargo clippy --locked --workspace --all-targets -- -D warnings && cargo nextest run --locked --workspace && cargo test --locked --workspace --doc && cargo deny check`
  - `test`: `cargo nextest run --workspace`
  - `mutants`: `cargo mutants -p syncfast-core`
  - `capture-fixtures`: roda `crates/syncfast-core/tests/capture_fixtures.sh` (passo 2)
  - `dev-env`: cria `./.dev/{config/syncfast,state,data,run,remote}` e grava `./.dev/rclone.conf` com `[devremote]\ntype = alias\nremote = <repo>/.dev/remote\n`, para que `Entire devremote` seja `./.dev/remote`
  - `run-gui-dev`: `dev-env`, depois `env XDG_CONFIG_HOME=$PWD/.dev/config XDG_STATE_HOME=$PWD/.dev/state XDG_DATA_HOME=$PWD/.dev/data XDG_RUNTIME_DIR=$PWD/.dev/run RCLONE_CONFIG=$PWD/.dev/rclone.conf SYNCFAST_SYSTEMD=off SYNCFAST_CLI=$PWD/target/debug/syncfast cargo run -p syncfast-gui`
  - `run-cli-dev *args`: o mesmo `env`, com `cargo run -p syncfast-cli -- {{args}}`
  - `dev-reset`: `rm -rf .dev`
  - `flatpak`: `flatpak-builder --user --install` do manifesto `packaging/flatpak/io.github.raphael_soares.syncfast.yml` (ADR 0006); é a única instalação, não há `cargo install`
  - `flatpak-bundle`: `flatpak build-bundle` produz `syncfast.flatpak`, para `flatpak install --user syncfast.flatpak` em outra máquina
  - `flatpak-sources`: regenera `packaging/flatpak/cargo-sources.json` com `flatpak-cargo-generator`
  - `hooks`: `lefthook install`
- `.gitignore`: `/target`, `/.dev`, `/mutants.out*`, `/syncfast.flatpak`.
- `README.md`:
  - Uma frase do que o app faz.
  - Seção Install com `just flatpak` (build local) e `flatpak install --user syncfast.flatpak` (bundle de `just flatpak-bundle`).
  - Seção Development com `sudo pacman -S --needed just lefthook cargo-deny`, `cargo install --locked cargo-mutants cargo-nextest`, `just hooks`, `just check`, `just run-gui-dev`.
  - Se a compilação acusar lib nativa faltando, instalar com `pacman -S --needed` a do erro. Candidatas: `libxkbcommon wayland libx11 libxcursor libxrandr libxi mesa fontconfig dbus`.
- `AGENTS.md` (até 40 linhas):
  - fronteira core/cli/gui
  - `ui/` só desenha e emite `Action`
  - I/O só no backend
  - nenhuma cor literal fora de `theme.rs`, exceto o ícone da bandeja
  - nunca logar token nem saída de `rclone config dump`
  - todo texto de UI em inglês, Title Case em botões
  - sem travessão
  - definition of done = `just check` verde mais teste do comportamento novo
  - decisões estruturais vão em `docs/adr/`
- `docs/adr/`, ADRs curtos (contexto, decisão, consequências; até 30 linhas cada):
  - `0001-workspace-core-cli-gui.md`
  - `0002-bisync-safety-invariants.md`: flags fixos, revisão obrigatória, sem exclude-if-present, versions
  - `0003-systemd-as-scheduler.md`
  - `0004-fastframe-visual-apple-hig-structure.md`
- Os três crates com `[lints] workspace = true`.
- Dependências por crate (versões no workspace):
  - `syncfast-core`: `tokio` (features `rt`, `process`, `fs`, `io-util`, `time`, `signal`, `macros`, `sync`), `serde` (derive), `serde_json`, `toml`, `toml_edit`, `thiserror`, `jiff`, `sha2`, `globset`, `regex`, `inotify` (feature `stream`), `zbus` (5.x, feature `tokio`), `log`, `fastframe-file`, `fastframe-instance`, `fastframe-notifications` (estes três em `git = "https://github.com/crmne/fastframe", tag = "v0.15.6"`). Dev: `tempfile`, `cargo_metadata`.
  - `syncfast-cli`: `syncfast-core`, `tokio` (`rt`, `macros`), `clap` (derive), `anyhow`, `fastframe-log`.
  - `syncfast-gui`: `syncfast-core`, `tokio` (`rt-multi-thread`), `anyhow`, `egui = "0.36.2"`, `eframe = { version = "0.36.2", default-features = false, features = ["accesskit", "glow", "default_fonts", "wayland", "x11", "persistence"] }`, `egui_extras` (feature `svg`), `resvg`, `rfd` (default-features off, features `xdg-portal` e `wayland`), e `fastframe-shell`, `-tray`, `-fonts`, `-text` (feature `watch`), `-theme`, `-icons`, `-log` na mesma tag. Dev: `egui_kittest`.
- O teste de fronteira `crates/syncfast-core/tests/boundary.rs` usa `cargo_metadata` e falha se as dependências normais transitivas do `syncfast-core` incluírem `egui`, `eframe`, `winit` ou qualquer `fastframe-*` além de `fastframe-file`, `fastframe-instance`, `fastframe-notifications` e `fastframe-desktop` (dependência não-UI do `fastframe-notifications`). A mensagem lista o caminho de dependência.
- Se a resolução falhar com tag e patch juntos, remover o `[patch.crates-io]` inteiro (o fastframe funciona com egui e winit originais, segundo a doc), registrar no `AGENTS.md` e seguir.

### 2. Fixtures reais do rclone, antes do parser

O script `crates/syncfast-core/tests/capture_fixtures.sh` (bash, `set -euo pipefail`) cria um tempdir com `local/` e `remote/` e um `RCLONE_CONFIG` temporário vazio. Em cada cenário, roda `rclone bisync local remote` com os args fixos do passo 5 (caminhos do tempdir) e grava o log JSON em `crates/syncfast-core/tests/fixtures/rclone-1.75.2/<cenario>.jsonl` e o código de saída em `<cenario>.exit`.

Cenários:

| Cenário | O que faz |
|---|---|
| `resync_dry_run` | 2 arquivos só no local, 1 só no remoto, 1 diferente nos dois com o remoto mais novo; `--resync --resync-mode newer --dry-run` |
| `resync` | o mesmo sem `--dry-run` |
| `upload` | cria um arquivo local, rodada normal |
| `download` | cria um arquivo remoto, rodada normal |
| `conflict` | altera o mesmo arquivo nos dois lados, rodada normal |
| `overwrite_local` | altera um arquivo no remoto, rodada normal (testa `--backup-dir1`) |
| `too_many_deletes` | apaga 3 de 4 arquivos locais, rodada normal |
| `force_dry_run` | o mesmo com `--force --dry-run` |
| `all_files_changed` | pasta com um só arquivo, editado no local, rodada normal (o `Safety abort`) |
| `filters_changed` | troca o filters-file, rodada normal |
| `critical` | renomeia os `*.lst` do workdir para `*.lst-err` antes da rodada |
| `stats` | 200 arquivos de 64 KiB e `--stats 1s`, para pegar linhas de stats |

- Commitar os fixtures.
- O passo 5 escreve os parsers contra esses arquivos, nunca contra texto suposto.
- Se o texto `too many deletes` ou `Must run --resync to recover` não aparecer no fixture correspondente, a classificação usa só código de saída e `*.lst-err` (ver passo 5), e o texto encontrado entra como sinal secundário.

### 3. Config e `Behavior` (`syncfast-core/src/config.rs`, `behavior.rs`)

- Arquivo `$XDG_CONFIG_HOME/syncfast/config.toml`. Ausente equivale a config vazio. Schema da fase 1 (serde com `deny_unknown_fields`):

  ```toml
  version = 1

  [[pair]]
  id = "drive"
  name = "Google Drive"
  local = "~/Drive"
  remote = "gdrive:"
  enabled = true
  presets = ["system_junk", "dev_dirs"]
  filters = []
  ```

- Erros globais (`GlobalConfigError`), que impedem carregar qualquer par:
  - TOML inválido: `config.toml: <mensagem do toml>`.
  - Chave desconhecida: `unknown key "<key>": not supported by this syncfast version`.
  - `version` diferente de 1: `unsupported config version <n>`.
- Erros por par (`PairError { pair_id, message }`). O par inválido fica fora do `apply` e os demais seguem:
  - `id` fora de `^[a-z0-9][a-z0-9-]{0,31}$`: `sync id "<id>" must use a-z, 0-9 and "-", up to 32 characters`.
  - `id` repetido: `duplicate sync id "<id>"` (os dois pares ficam inválidos).
  - `name` vazio ou com caractere de controle: `sync name must be one line of text`.
  - `local` relativo depois de expandir `~`: `local folder must be an absolute path`.
  - `local` contendo `$XDG_DATA_HOME/syncfast`: `local folder cannot contain syncfast's versions folder`.
  - Sobreposição local (canonicalizar quando existe; igualdade ou prefixo por componente): `local folder overlaps "<outro name>"`.
  - Sobreposição remota (mesmo nome de remote; caminho igual ou prefixo por componente; raiz sobrepõe tudo): `remote folder overlaps "<outro name>"`.
  - Remote fora de `rclone listremotes` (que respeita `RCLONE_CONFIG`): `account "<name>" is not set up in rclone`.
  - Filtro que não começa com `+ ` ou `- `: `filter line <n> must start with "+ " or "- "`.
  - Preset desconhecido: `unknown preset "<name>"`.
- Pasta local inexistente não é erro de config. É estado de rodada (`local_missing`, passo 7), porque um disco pode estar desmontado.
- `Behavior`, com estes campos e padrões:

  | Campo | Padrão |
  |---|---|
  | `watch` | `true` |
  | `debounce` | `30s` |
  | `max_wait` | `5m` |
  | `interval` | `5m` |
  | `conflict_resolve` | `ConflictResolve::Newer` |
  | `conflict_loser` | `ConflictLoser::Num` |
  | `versions` | `true` |
  | `versions_keep_days` | `30` |
  | `google_docs` | `GoogleDocs::Skip` |
  | `max_delete` | `50` |
  | `resync_mode` | `ResyncMode::Newer` |
  | `empty_dirs` | `true` |

  - Os enums nascem com todas as variantes da fase 2: `ConflictResolve { None, Newer, Older, Larger, Smaller, Local, Remote }`, `ConflictLoser { Num, Pathname, Delete }`, `GoogleDocs { Skip, Export, Url }`, `ResyncMode { Newer, Older, Larger, Smaller, Local, Remote }`. `Local` vira `path1` e `Remote` vira `path2`.
  - `Behavior::for_pair(&Pair) -> Behavior` devolve `Behavior::default()` na fase 1.
  - Todo consumidor (args do bisync, render das units, watcher, poda de versões) lê desses campos; nenhum valor é repetido como literal.
  - Não existe campo `review` na fase 1.
- Escrita: `save(&Config)` canonicaliza o caminho (segue o symlink do stow) e edita com `toml_edit`, para preservar comentários e ordem. Grava com `fastframe_file::write` dentro de `tokio::task::spawn_blocking`.

### 4. Filtros (`filters.rs`)

- Presets (constantes):
  - `system_junk`: `- .directory`, `- .Trash-*/`, `- .DS_Store`, `- .AppleDouble`, `- .LSOverride`, `- ._*`, `- Thumbs.db`, `- ehthumbs.db`, `- desktop.ini`, `- $RECYCLE.BIN/`, `- ~$*`, `- *.tmp`, `- *.temp`, `- *.part`, `- *.crdownload`, `- *.log`, `- .cache/`
  - `dev_dirs`: `- .git/`, `- node_modules/`, `- target/`, `- .venv/`, `- __pycache__/`
- `render(&Pair) -> String`: linha `# Generated by syncfast. Do not edit.`, depois `filters` na ordem, depois os presets ligados na ordem `system_junk`, `dev_dirs`. Termina com `\n`.
- Gravação em `$XDG_STATE_HOME/syncfast/filters/<id>.txt` só quando o conteúdo muda. O `.md5` do bisync fica ao lado.
- `hash(&str) -> String`: SHA-256 em hex.
- `DirMatcher::new(rendered) -> DirMatcher` e `is_excluded(rel_dir: &Path) -> bool`:
  - Só as linhas `- <padrão>/` contam.
  - Com `/` inicial, o padrão é ancorado na raiz do par. Sem `/` inicial, casa o sufixo do caminho por segmentos em qualquer profundidade.
  - `*` e `?` não atravessam `/`; `**` atravessa.
  - Compila com `globset` e `literal_separator(true)`.
  - Se houver qualquer linha `+ `, `is_excluded` sempre devolve `false`.

### 5. rclone e bisync (`rclone.rs`, `bisync.rs`)

- Binário do rclone: se `SYNCFAST_RCLONE` estiver definida, usa esse caminho absoluto. Senão, procura `rclone` no PATH. Se não encontrar, o erro é `RcloneNotFound` e a mensagem `rclone not found; install rclone or reinstall syncfast units`.
- `bisync_args(pair, behavior, paths, mode, now) -> Vec<OsString>`, em ordem fixa:

  ```
  bisync <local> <remote>
  --filters-file <state>/filters/<id>.txt
  --workdir <state>/bisync/<id>/
  --resilient --recover --max-lock 2m
  --conflict-resolve newer --conflict-loser num
  --max-delete 50
  --create-empty-src-dirs
  --drive-skip-gdocs
  --backup-dir1 <data>/versions/<id>/<AAAAMMDD-HHMMSS>/
  --stats 2s --stats-log-level NOTICE
  --use-json-log --log-file <state>/pairs/<id>/runs/<AAAAMMDD-HHMMSS>.jsonl -v
  ```

  - O timestamp vem de `now` (relógio injetado), no fuso local.
  - `mode`:
    - `Normal`: nada a mais.
    - `Resync`: `--resync --resync-mode newer`.
    - `Force`: `--force`.
    - `DryRun(Resync)` e `DryRun(Force)`: o modo interno mais `--dry-run`, com `--log-file` num arquivo temporário.
  - Teste unitário com snapshot do vetor por modo.
- `classify(exit_code, log_lines, workdir) -> Outcome`. A ordem dos testes é esta:
  1. Código 0: `Ok`.
  2. Existe `*.lst-err` em `<workdir>`, ou `<workdir>` não pode ser lido por outro motivo que não ausência: `NeedsReview(CriticalError)`.
  3. Alguma linha casa com o padrão de autenticação (`invalid_grant`, `couldn't fetch token`, `oauth2: cannot fetch token`, `token expired`, sem diferenciar maiúsculas): `AuthError`. Se um erro de autenticação real aparecer com outro texto, acrescentar o texto a essa lista com teste.
  4. Código 1 e alguma linha com `too many deletes`: `TooManyDeletes`.
  5. Código 1 e alguma linha error+ com `Safety abort: all files were changed`: `AllFilesChanged`.
  6. Alguma linha casa com um `Fault` que não é autenticação (`fault_of`: `Account`, `Quota`, `Permission`, `RemoteMissing`, `LocalEmpty`): `NeedsAttention { fault, detail }`, com `detail` a mensagem que casou. `TooManyDeletes` e `AllFilesChanged` têm prioridade.
  7. Código 7 e alguma linha com `Must run --resync to recover`: `NeedsReview(CriticalError)`. Um código 7 que o log diz ser repetível sem `--resync` (`critical_retryable`) cai em `Transient`.
  8. Código 2: `ConfigError` (bug de flags do syncfast).
  9. Qualquer outro caso: `Transient`.
  - Um teste por ramo, usando os fixtures do passo 2.
- `summarize(log_lines) -> RunSummary`:
  - Contadores `uploaded`, `downloaded`, `deleted_local`, `deleted_remote`, `conflicts_new` (todos `Option<u32>`) e `changes: Vec<Change { kind, path }>`.
  - `kind` ∈ `Upload`, `Download`, `OverwriteLocal`, `OverwriteRemote`, `DeleteLocal`, `DeleteRemote`, `Other`.
  - O mapeamento mensagem → kind é escrito olhando os fixtures `resync_dry_run`, `force_dry_run`, `upload`, `download`, `conflict`, `overwrite_local` e `too_many_deletes`, e testado contra eles.
  - Linha NOTICE ou maior sem padrão conhecido vira `Other` com o `msg` cru.
  - Contador que não pode ser obtido com segurança fica `None`.
- `last_stats(log_lines) -> Option<Progress>`: lê a última linha com objeto `stats` e devolve `{ transfers, total_transfers, bytes, total_bytes, speed, eta_seconds: Option<u64>, current: Option<String> }`. Os nomes dos campos são os do fixture `stats`.
- Wrappers de rclone (`std::process`/`tokio::process`, args explícitos, sem shell, stdin `Stdio::null()`):
  - `listremotes() -> Vec<String>`.
  - `drive_remotes_without_own_client_id() -> Vec<String>`: `rclone config dump`, parse em memória, `type == "drive"` com `client_id` ausente ou vazio. A saída nunca é logada nem guardada.
  - `create_drive_remote(name, client_id, client_secret) -> Child`: `rclone config create <name> drive client_id=<id> client_secret=<secret> scope=drive config_is_local=true`.
  - `update_drive_credentials(name, client_id, client_secret) -> Child`: `rclone config update <name> client_id=<id> client_secret=<secret>`.
  - `list_dirs(remote_path) -> Vec<String>`: `rclone lsjson --dirs-only --no-modtime --no-mimetype <remote_path>`.
  - `list_files(path, filters: Option<&Path>) -> Vec<String>`: `rclone lsf -R --files-only [--filter-from <f>] <path>`.

### 6. Estado (`state.rs`)

- `$XDG_STATE_HOME/syncfast/pairs/<id>/state.json`:

  ```json
  {
    "gate": null,
    "last_outcome": null,
    "offline": false,
    "consecutive_failures": 0,
    "failing_since": null,
    "last_success": null,
    "last_attempt": null,
    "applied_filters_hash": null,
    "conflicts_pending": 0,
    "last_run": null,
    "notified": []
  }
  ```

  - `gate`: `null` ou um objeto `{ "kind": ..., "since": "<rfc3339>", "detail": "<texto>" }`, mais `"fault"` (`auth`, `account`, `quota`, `permission`, `remote_missing` ou `local_empty`) no `needs_attention` que um erro de rclone causou; arquivos antigos não o têm. O `kind` ∈ `needs_review_first_run`, `needs_review_filters_changed`, `needs_review_critical_error`, `too_many_deletes`, `all_files_changed`, `config_error`, `auth_error`, `local_missing`, `needs_attention`. Todo gate bloqueia rodadas normais, exceto `needs_attention`: a próxima rodada roda no ritmo do timer e um `Ok` o limpa.
  - `last_outcome` ∈ `null`, `ok`, `error`.
  - `offline`: true quando a última tentativa foi pulada por falta de rede. Não muda `gate` nem `last_outcome`.
  - `last_run`: `{ "started", "finished", "exit_code", "summary": RunSummary, "log_file" }`.
  - `notified`: lista de chaves já notificadas (`"gate:<kind>"`, `"failures"`), para deduplicar.
  - Arquivo ausente equivale a `gate = needs_review_first_run`.
- O estado nunca guarda "running" nem "paused":
  - Em execução = o lock do par está ocupado. `is_running(id)` tenta `File::try_lock_shared` no arquivo de lock e solta em seguida.
  - Pausado = `enabled = false` no config.
- Estado exibido (`display_status(config_pair, state, running) -> DisplayStatus`), primeira regra que casar:

  | Ordem | Condição | Status | Cor |
  |---|---|---|---|
  | 1 | `running` | `Syncing` | `accent` |
  | 2 | gate `config_error`, `auth_error`, `local_missing`, `needs_attention` | `Error` | `danger` |
  | 3 | gate `needs_review_*`, `too_many_deletes`, `all_files_changed` | `NeedsReview` | `warning` |
  | 4 | `conflicts_pending > 0` | `Conflicts` | `warning` |
  | 5 | `!enabled` | `Paused` | `dim` |
  | 6 | `offline` | `Offline` | `dim` |
  | 7 | `last_outcome = error` e `consecutive_failures >= 3` | `Failing` | `danger` |
  | 8 | `last_outcome = error` | `Retrying` | `secondary` |
  | 9 | senão | `UpToDate` | `secondary` |

  A cor nunca é o único sinal: cada status tem ícone Lucide e texto.

  | Status | Ícone | Texto |
  |---|---|---|
  | Syncing | `refresh-cw` | `Syncing…` |
  | Error | `circle-alert` | `Needs attention` |
  | NeedsReview | `eye` | `Review needed` |
  | Conflicts | `copy` | `<n> conflicts` |
  | Paused | `pause` | `Paused` |
  | Offline | `cloud-off` | `Offline` |
  | Failing | `circle-alert` | `Sync failing` |
  | Retrying | `clock` | `Will retry` |
  | UpToDate | `check` | `Up to date` |

  Ícone sem equivalente no set Lucide do fastframe entra como SVG próprio em `crates/syncfast-gui/assets/icons/`, copiado do Lucide (ISC).
- Pasta `runs/`: no fim de cada `run`, ficam as 50 rodadas mais recentes por nome; as outras são apagadas.
- Marcador `pairs/<id>/confirm.json` `{ "kind": "resync" | "force", "filters_hash": "<hash>" }`: a GUI grava e o `run` consome.
- Toda gravação de estado usa `fastframe_file::write`.

### 7. Decisão e execução de rodada (`run_decision.rs`, `run.rs`, CLI `syncfast run <id>`)

`decide(input) -> Decision` é função pura. A entrada é `{ pair_enabled, local_exists, online, gate, applied_filters_hash, current_filters_hash, marker }`. Ela devolve `Skip(reason)`, `SetGate(kind)` ou `Run(mode)`. A primeira linha que casar vale:

| # | Condição | Decisão |
|---|---|---|
| 1 | `!pair_enabled` | `Skip(Paused)` |
| 2 | `!local_exists` e gate `null` ou `local_missing` | `SetGate(local_missing)` |
| 2b | `!local_exists` e qualquer outro gate | `Skip(Gated)`, o gate fica |
| 3 | gate `local_missing` e local existe | trata como gate `null` e continua |
| 3b | gate `config_error` (o par é válido, senão o `run` nem chega aqui) e `applied_filters_hash` não nulo | trata como gate `null` e continua |
| 3c | gate `config_error` e `applied_filters_hash` nulo (nunca sincronizou) | `SetGate(needs_review_first_run)` |
| 4 | `!online` | `Skip(Offline)` |
| 5 | marker `resync` e gate ∈ `needs_review_*` e `marker.filters_hash == current` | `Run(Resync)` |
| 6 | marker `force` e gate `too_many_deletes` ou `all_files_changed` e `marker.filters_hash == current` | `Run(Force)` |
| 7 | marker presente sem casar com 5 ou 6 | descarta o marker e continua |
| 8 | gate não nulo (exceto `local_missing` já tratado) | `Skip(Gated)` |
| 9 | `applied_filters_hash` não nulo e diferente de `current` | `SetGate(needs_review_filters_changed)` |
| 10 | senão | `Run(Normal)` |

- `config_error` e `auth_error` não têm marker. `config_error` sai sozinho na próxima rodada com config válida (linhas 3b e 3c), mesmo sem GUI. `auth_error` sai pela GUI: ela limpa o gate ao reconectar a conta e então chama `Sync Now`.
- Um gate nunca é sobrescrito por `local_missing` (linha 2b): uma revisão pendente (inclusive a primeira) sobrevive à pasta sumir e voltar. Quando `local_missing` ou `config_error` terminam, a chave `gate:<kind>` sai de `notified`, para a próxima ocorrência notificar de novo.
- Mudar o filtro com gate `too_many_deletes` ou `all_files_changed` cai na linha 6 com hash diferente, depois na 7 e na 8. O gate continua até a próxima revisão.
- Para isso não travar, a GUI, ao salvar filtros, troca qualquer gate `too_many_deletes` ou `all_files_changed` por `needs_review_filters_changed`.
- Teste unitário para cada linha da tabela, mais o caso "needs_review → pausa → retoma → ainda pede revisão".

Fluxo do `run`:

1. Carregar config. Erro global ou `id` inexistente: imprimir no stderr e sair 2. Par inválido: gravar gate `config_error` com a mensagem, notificar se for transição, sair 0.
2. `File::try_lock` exclusivo em `$XDG_RUNTIME_DIR/syncfast/<id>.lock` (cria o diretório). Ocupado: sair 0.
3. Rede: ler `State` (u32) de `org.freedesktop.NetworkManager` em `/org/freedesktop/NetworkManager` no barramento de sistema. `online = State >= 70`. Sem NetworkManager no barramento, `online = true`. A rede fica atrás do trait `Network` para teste.
4. Renderizar e gravar o filters-file. Calcular `current_filters_hash`. Ler o `state.json`: se não for JSON válido, renomeá-lo para `state.json.corrupt-<AAAAMMDD-HHMMSS>`, avisar no log e seguir como primeira execução (a revisão obrigatória protege os dados).
5. `decide(...)`:
   - `Skip(Offline)` grava `offline = true`.
   - `Skip(Paused)` e `Skip(Gated)` não gravam nada.
   - `SetGate` grava o gate e notifica se for transição.
   - Em todos esses casos, sair 0.
6. `Run(mode)`: gravar `last_attempt` e `offline = false`, iniciar o rclone como filho (stdout e stderr descartados, o log vai para `--log-file`), esperar.
   - SIGINT/SIGTERM via `tokio::signal::unix`: o `run` não morre; repassa SIGINT ao filho (o systemd já manda ao cgroup inteiro) e espera.
7. `classify` e `summarize`. Se a rodada `Normal` saiu 7 com `Empty prior Path1 listing` ou `Empty prior Path2 listing`, refazer na mesma execução uma rodada `Resync` (uma tentativa só, sem revisão: o resync só copia) e classificar a segunda. Depois:
   - `Ok`:
     - `gate = null`, `last_outcome = ok`, `consecutive_failures = 0`.
     - `applied_filters_hash = current`, `last_success = agora`.
     - Remover `"failures"` e todas as chaves `gate:*` de `notified`.
     - Apagar o marker.
     - `conflicts_pending` = número de arquivos `*.conflict<N>` na árvore local, varrida com o `DirMatcher`.
     - Se `summary.conflicts_new > 0`, notificar conflitos.
   - `TooManyDeletes`, `AllFilesChanged`, `NeedsReview(CriticalError)`, `AuthError`, `ConfigError`: gravar o gate correspondente e notificar se for transição.
   - `NeedsAttention`: gate `needs_attention` com o `detail` e o `fault`, conta como falha (`last_outcome = error`, `consecutive_failures += 1`) e notifica se for transição. O marker é mantido, como no `Transient`: uma primeira rodada confirmada que falha assim volta a rodar no próximo tick em vez de voltar à primeira revisão.
   - `Transient`: `last_outcome = error`, `consecutive_failures += 1`. Ao chegar em 3 sem `"failures"` em `notified`, notificar e acrescentar `"failures"`. A primeira falha de uma série grava `failing_since`; qualquer resultado que não seja `Transient` nem `NeedsAttention`, e um tick `Offline`, o limpa. Sem gate e com `failing_since` há mais de 6 horas, gravar o gate `needs_attention` com `Syncing has failed for over 6 hours: <última mensagem>` e notificar se for transição.
8. Depois de toda rodada, qualquer que seja o resultado, podar versões: em `versions/<id>/`, apagar os subdiretórios diretos cujo nome casa `^\d{8}-\d{6}$` e cuja data é mais antiga que `versions_keep_days`. Tudo o mais em `versions/<id>/` fica intocado. Remover diretórios datados que ficarem vazios. Podar `runs/`. Uma falha ao podar é só um aviso no log, nunca o resultado da rodada.
9. Sair 0. Erro interno do próprio syncfast (I/O de estado, por exemplo): log e sair 1.

Notificações (`notify.rs`, no core):

- Primeiro, `fastframe_instance::Slot::new(SLOT).send("notify <id>")`, com `SLOT = "syncfast"` como constante pública do core. Se a GUI responder, ela mostra a notificação com clique.
- Se `send` falhar, o `run` cria um `fastframe_notifications::Client` (`App::new("syncfast", "syncfast").desktop_entry("syncfast").icon("syncfast")`) e mostra a notificação sem ação.
- O `run` nunca chama `claim`.
- "Transição" = a chave (`gate:<kind>`) não está em `notified`. Notificou, acrescenta.

| Evento | Título | Corpo |
|---|---|---|
| `needs_review_first_run` | `<name>: review needed` | `Review the first sync before it starts.` |
| `needs_review_filters_changed` | `<name>: review needed` | `Filters changed. Review what will sync.` |
| `needs_review_critical_error` | `<name>: review needed` | `Sync stopped after an error. Review to resume.` |
| `too_many_deletes` | `<name>: many deletions` | `More than half of the files would be deleted. Review before continuing.` |
| `all_files_changed` | `<name>: review needed` | `Every file changed at once. Review before continuing.` |
| `config_error` | `<name>: configuration error` | a mensagem |
| `auth_error` | `<name>: account disconnected` | `Reconnect "<remote>" in Settings > Accounts.` |
| `local_missing` | `<name>: folder not found` | `<local> is missing. Sync resumes when it is back.` |
| `needs_attention` | `<name>: needs attention` | `Account`: `Google Drive refused the account. Open syncfast for the fix.`; `Quota`: `Google Drive is full.`; `Permission`: `No permission to a file or folder on Google Drive.`; `RemoteMissing`: `The folder on Google Drive is missing.`; `LocalEmpty`: `The folder is empty. If it is on a drive, mount it.`; falha longa: `Syncing keeps failing. Open syncfast for details.` |
| conflitos | `<name>: <n> new conflicts` | `Both versions were kept. Open syncfast to resolve.` |
| falhas | `<name>: sync failing` | `Failed 3 times in a row. <msg>` |

### 8. Units e `syncfast apply` (`units.rs`)

- Diretório: `$XDG_CONFIG_HOME/systemd/user/`. Toda unit começa com `# Generated by syncfast. Do not edit; run "syncfast apply".`
- Escapes:
  - `<name>` em `Description=` troca `%` por `%%` (newline já é recusado pela validação).
  - Caminhos em `ExecStart=` e `Environment=` ficam entre aspas duplas, com `\` e `"` escapados e `%` duplicado.
  - Teste unitário com nome contendo `%` e caminho com espaço.
- `<bin>` = `std::env::current_exe()` canonicalizado. `<rclone>` = o rclone resolvido no momento do `apply`, canonicalizado.
- Dentro do Flatpak (`FLATPAK_ID` definido), `ExecStart=` vira `flatpak run --die-with-parent --command=syncfast <FLATPAK_ID> run <id>` (e `... watch`), sem a linha `Environment=`: o rclone embutido está no PATH do sandbox. O `flatpak run` move o sandbox para um scope próprio, então sem `--die-with-parent` um `stop`/`restart` da unit deixa o processo vivo. Os `systemctl --user` rodam por `flatpak-spawn --host` (ADR 0006).

`syncfast-<id>.service`:

```ini
# Generated by syncfast. Do not edit; run "syncfast apply".
[Unit]
Description=syncfast: <name>

[Service]
Type=oneshot
ExecStart="<bin>" run <id>
Environment="SYNCFAST_RCLONE=<rclone>"
KillSignal=SIGINT
TimeoutStartSec=infinity
TimeoutStopSec=180
Nice=10
IOSchedulingClass=idle
```

`syncfast-<id>.timer` (`<interval>` = `Behavior.interval` em segundos):

```ini
# Generated by syncfast. Do not edit; run "syncfast apply".
[Unit]
Description=syncfast timer: <name>

[Timer]
OnStartupSec=2min
OnUnitInactiveSec=<interval>s
Unit=syncfast-<id>.service

[Install]
WantedBy=timers.target
```

`syncfast-watch.service`:

```ini
# Generated by syncfast. Do not edit; run "syncfast apply".
[Unit]
Description=syncfast local change watcher

[Service]
ExecStart="<bin>" watch
Environment="SYNCFAST_RCLONE=<rclone>"
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

`apply(paths, systemd: SystemdMode) -> ApplyReport`, com `SystemdMode::{User, Off}` (`Off` quando `SYNCFAST_SYSTEMD=off`):

1. Lock exclusivo bloqueante em `$XDG_RUNTIME_DIR/syncfast/apply.lock`.
2. Carregar config. Erro global: relatório com o erro, nada alterado, CLI sai 2.
3. Remoções (`syncfast-<id>.*` cujo `id` não existe mais no config):
   1. `systemctl --user disable --now syncfast-<id>.timer` e `stop syncfast-<id>.service`.
   2. Lock bloqueante no lock do par, com timeout de 300 s. Se estourar, deixa a remoção para o próximo `apply` e registra no relatório.
   3. Apagar as units, `pairs/<id>/`, `filters/<id>.txt`, `filters/<id>.txt.md5`, `bisync/<id>/`.
   4. Apagar `versions/<id>/` só com `--delete-versions <id>`.
4. Pares inválidos: sem units (as existentes são removidas como no item 3.1 e 3.3, preservando `pairs/<id>/`). Gravar gate `config_error` no estado.
5. Pares válidos: renderizar e gravar só as units que mudaram.
6. Se algo mudou: `systemctl --user daemon-reload`.
7. Para cada par válido: `enabled` → `systemctl --user enable --now syncfast-<id>.timer`; senão `disable --now`.
8. `systemctl --user enable syncfast-watch.service`, depois `systemctl --user restart syncfast-watch.service`.
9. Falha de comando entra no relatório (`pair_id`, comando, stderr) e o `apply` continua.

A CLI imprime o relatório e sai 0 se ele estiver vazio, 1 se houver falhas. Em `SystemdMode::Off`, as units vão para `$XDG_STATE_HOME/syncfast/units-preview/` e nenhum `systemctl` roda.

A GUI chama `apply` como função do core: ao iniciar, depois de cada gravação de config, e no Pause/Resume.

### 9. `syncfast watch` (`watch.rs`)

- Carrega o config. Para cada par válido com `enabled` e `Behavior.watch`, percorre a árvore local e põe um watch inotify não recursivo (`CREATE | DELETE | MODIFY | MOVED_FROM | MOVED_TO | CLOSE_WRITE | DELETE_SELF`) em cada diretório que o `DirMatcher` não exclui.
- Eventos:
  - `CREATE|ISDIR` e `MOVED_TO|ISDIR`: varrer o diretório novo recursivamente e pôr watches.
  - `IGNORED`: remover o descritor do mapa.
  - `Q_OVERFLOW`: disparar todos os pares e refazer todos os watches.
- Disparo por par:
  - Cada evento reinicia um timer de `debounce`. Se o primeiro evento pendente tiver mais de `max_wait`, dispara mesmo assim.
  - Na hora de disparar: se `is_running(id)`, marca `dirty` e checa o lock a cada 5 s; quando ficar livre, espera mais `debounce` e dispara uma vez. Eventos durante a rodada só marcam `dirty`, para não reagir às escritas do próprio rclone.
  - Disparo = `systemctl --user start --no-block syncfast-<id>.service`. Em `SystemdMode::Off`, roda `syncfast run <id>` como filho.
- Recarga: watch no diretório pai do caminho **canonicalizado** do `config.toml` (o alvo do symlink do stow). Mudança no arquivo refaz tudo.
- `ENOSPC` ao pôr watch: log `inotify watch limit reached for "<id>"; relying on the timer` e para de pôr watches nesse par.

### 10. `preview` e `status` (CLI, sobre o core)

- `syncfast preview <id> --kind resync|force`:
  - `try_lock` no lock do par. Ocupado: sai 3 com `this sync is running; try again when it finishes`.
  - Roda `DryRun(...)` e imprime JSON `{ "filters_hash": "...", "changes": [...], "counts": {...} }`.
  - A GUI chama a mesma função do core diretamente.
- `syncfast status`: uma linha por par: `<id>\t<DisplayStatus>\t<last_success ou never>`.
- `clap`: subcomandos `apply [--delete-versions <id>]`, `run <id>`, `watch`, `preview <id> --kind <k>`, `status`. Sem subcomando: imprime a ajuda.

### 11. Design system (`syncfast-gui/src/theme.rs`, `ui/widgets/`)

- `Palette` com os campos e o formato JSON do spotifast (`src/theme.rs`): `dark` e os 16 tokens. Implementa `fastframe_theme::Palette`, com catálogo e temas de usuário em `$XDG_CONFIG_HOME/syncfast/themes/`, incluindo os erros de tema exibidos em Settings > Appearance.
- Paletas embutidas (neutros do spotifast, acento violeta):

  | Token | Dark | Light |
  |---|---|---|
  | window | `#0F1114` | `#F8F9FB` |
  | panel | `#15181C` | `#FFFFFF` |
  | surface | `#1D2127` | `#EEF0F3` |
  | surface_hover | `#262B33` | `#E3E6EB` |
  | surface_active | `#2F353F` | `#D7DBE1` |
  | outline | `#2A3038` | `#DDE1E6` |
  | text | `#F2F4F6` | `#14171A` |
  | secondary | `#A9B1BC` | `#535B66` |
  | dim | `#6E7784` | `#8B939E` |
  | accent | `#A78BFA` | `#7C3AED` |
  | accent_hover | `#C4B5FD` | `#6D28D9` |
  | on_accent | `#1E1033` | `#FFFFFF` |
  | danger | `#F5717F` | `#D63B4C` |
  | warning | `#F2B85C` | `#B87A14` |
  | overlay | `#22272E` | `#FFFFFF` |
  | shadow | `#0000008C` | `#00000032` |

- `apply_to_style(ctx, &Palette)`: copiar a função de mesmo nome do spotifast, com os valores que ele usa:
  - fills por estado (inactive `surface`, hovered `surface_hover`, active `surface_active`)
  - `selection.bg_fill = accent.gamma_multiply(0.35)`
  - `window_shadow` offset [0,6] e blur 24
  - `item_spacing (8, 6)`, `button_padding (12, 6)`, `interact_size (40, 28)`
  - scrollbars flutuantes 6 px
- Tipografia: escala do HIG macOS em Inter. Helpers em `theme.rs`:

  | Estilo | Tamanho | Peso | Uso |
  |---|---|---|---|
  | `large_title` | 26 | Bold | boas-vindas |
  | `title2` | 17 | SemiBold | título do detalhe e de sheet |
  | `title3` | 15 | SemiBold | título de seção |
  | `headline` | 13 | SemiBold | rótulo de linha, botão |
  | `body` | 13 | Regular | texto |
  | `callout` | 12 | Regular | texto de apoio |
  | `subheadline` | 11 | Regular | metadados de linha |
  | `footnote` | 10 | Regular | mínimo; só legendas técnicas |

  `egui::TextStyle`: `Body = body`, `Button = headline`, `Heading = title2`, `Small = subheadline`, `Monospace = 12` regular.
- Medidas:
  - Espaçamento em múltiplos de 4: `XS 4`, `S 8`, `M 12`, `L 16`, `XL 20`, `XXL 24`.
  - Margem interna de janela e de sheet: 20.
  - Raio: controles 6, cartões/linhas 8, janela/sheet 10.
  - Altura de controle: 28. Linha da sidebar: 32. Linha de lista de duas linhas: 44.
  - Sidebar: padrão 240, faixa 200 a 320, redimensionável.
  - Janela principal: padrão 960×640, mínimo 760×520.
  - Sheet: largura 560, altura até 80% da janela, com scroll interno.
- Componentes, em `ui/widgets/`, só com tokens:
  - `primary_button` (fill `accent`, texto `on_accent`), `secondary_button` (fill `surface`), `destructive_button` (texto `danger`, nunca default), `icon_button` (com tooltip obrigatório)
  - `sidebar_row`, `list_row`, `status_badge` (ícone + texto + cor da tabela do passo 6)
  - `segmented` (abas do detalhe), `toolbar`
  - `sheet` (overlay `overlay`, título `title2`, corpo, fileira de botões no rodapé: Cancel à esquerda do primário, primário à direita, Enter aciona o primário, Esc o Cancel)
  - `alert` (título, texto informativo, até 3 botões)
  - `progress_bar` (determinada e indeterminada), `inline_error` (texto `danger` com ícone `circle-alert`, logo abaixo do campo)
  - `empty_state` (ícone 40, título `title3`, texto `callout`, botão)
  - `text_field`, `text_area`, `switch` (40×22), `folder_tree`
- Ícones: `fastframe_icons::icons!` com as variantes Lucide usadas (tabela do passo 6 mais `folder`, `folder-plus`, `folder-sync`, `settings`, `trash-2`, `external-link`, `chevron-left`, `chevron-right`, `x`, `plus`, `user`, `palette`, `filter`, `history`). Ícone fora do set compartilhado vira SVG próprio no diretório de assets.
- Ícone da bandeja (HIG: símbolo):
  - Glifo monocromático Lucide `folder-sync`, rasterizado com `resvg` dentro do `DrawIcon`, com a cor de `IconColour` escolhida conforme o painel claro ou escuro (como o zapfast faz em `tray_colour`).
  - Estado:
    - `Syncing` troca o glifo por `refresh-cw`.
    - `Offline` troca por `cloud-off`.
    - Atenção e erro acrescentam um ponto de 35% do tamanho no canto inferior direito, com as cores fixas `#F2B85C` (atenção) e `#F5717F` (erro). São as únicas cores literais fora da paleta.
  - Pior estado entre pares, do pior para o melhor: `Error`, `NeedsReview`, `Conflicts`, `Failing`, `Syncing`, `Paused`, `Offline`, `Retrying`, `UpToDate`. O que exige ação do usuário vem antes de rodada em andamento, pausa e falta de rede (a ordem do passo 6 vale para um par só).
- Template Noctalia em `contrib/noctalia/syncfast/` com `template.toml`, `syncfast-dark.json`, `syncfast-light.json`. Cópia do spotifast trocando `spotifast` por `syncfast`. `output_path = "$XDG_CONFIG_HOME/syncfast/themes/noctalia.json"`, `post_hook = "flatpak run io.github.raphael_soares.syncfast reload-themes >/dev/null 2>&1 || true"`, `requires_path = "$XDG_CONFIG_HOME/syncfast"`. 

### 12. Casca da GUI (`main.rs`, `app.rs`, `backend.rs`)

- Estrutura do zapfast:
  - `fastframe_log::Logging::new("syncfast-gui", env!("CARGO_PKG_VERSION")).init()`.
  - `fastframe_shell::Shell::new(app, &waker).start_hidden(flag).idle(fastframe_tray::idle).run(...)`.
  - `impl fastframe_shell::Resident for App`: pedido de fechar janela → `Closed::Hide`; Quit → `Quit`.
  - Fontes: `fastframe_fonts::FontSetup::default()` com `fastframe_text::detect()` e `watch`.
  - Geometria: `fastframe_shell::window::Geometry`, persistida em `$XDG_STATE_HOME/syncfast/gui.json` junto com `welcome_done`, `selected_pair`, `sidebar_width` e `theme` (`"system"`, `"light"`, `"dark"` ou nome de arquivo em `themes/`).
- Antes de codar, ler as assinaturas reais do fastframe v0.15.6. Pontos não confirmados:
  - `Tray::events` com `&self` ou `&mut self`
  - o tipo `Handle` de `Client::show_with`
  - o parâmetro de `Waker::attach`
- Argumentos: sem argumento abre a janela; `--start-hidden` inicia só na bandeja; `reload-themes` manda `reload-themes` ao slot e sai (sem instância rodando: `not running`, sai 1).
- Instância única (`Slot::new(SLOT).claim(...)`):
  - pedidos `show`, `notify <id>`, `reload-themes`
  - segunda execução com `Claim::Running` manda `show` e sai
- `notify <id>`:
  - Relê o estado e mostra a notificação da tabela do passo 7.
  - `Event::Activated` abre a janela com o par selecionado e, se o par tiver gate de revisão, abre a sheet Review.
- Bandeja (`fastframe_tray`, HIG: sempre os mesmos itens, desabilitar em vez de esconder):
  1. Linha de status desabilitada: `All syncs up to date` / `Syncing <n>…` / `<n> syncs need attention` / `Offline` / `All syncs paused` / `No syncs yet`.
  2. Separador.
  3. `Show syncfast` / `Hide syncfast`, conforme a janela.
  4. `Sync All Now` (desabilitado sem pares).
  5. `Pause All` / `Resume All`, conforme o estado: se qualquer par estiver ativo, mostra `Pause All`. Desabilitado sem pares.
  6. Separador.
  7. `Settings…`
  8. `Quit syncfast`
  - Clique esquerdo (`Event::Toggle`) alterna a janela.
- Backend:
  - Tarefa que relê `pairs/*/state.json`, `is_running` e, durante uma rodada, `last_stats` do log dela. Intervalos: 1 s com janela visível e rodada ativa, 2 s com janela visível, 10 s escondida.
  - Toda chamada a rclone, systemctl e arquivo passa por `Command`/`Event`.
  - `Sync Now`: `systemctl --user start --no-block syncfast-<id>.service`. Em `SYNCFAST_SYSTEMD=off`, roda `$SYNCFAST_CLI run <id>` como filho.
- Atalhos:
  - `Ctrl+N`: Add Sync…
  - `Ctrl+R`: Sync Now no par selecionado
  - `Ctrl+,`: Settings
  - `Ctrl+W`: esconder a janela
  - `Ctrl+Q`: Quit
  - `Esc` cancela a sheet; `Enter` aciona o botão primário da sheet.

### 13. Telas (`syncfast-gui/src/ui/`)

Toda tela é função de um `ViewModel` imutável e devolve `Vec<Action>`. O `app.rs` aplica as ações depois do frame. Isso deixa todas as telas testáveis com `egui_kittest` sem backend.

**Janela principal (split view):**

- Título da janela: nome do par selecionado; sem seleção, `Syncs`.
- **Sidebar:**
  - Cabeçalho `Syncs` com três `icon_button` à direita: `plus` (tooltip `Add Sync…`), `settings` (tooltip `Settings…`) e `ellipsis` (tooltip `syncfast Menu`). O menu tem as mesmas linhas da bandeja que não dependem da janela: `Sync All Now`, `Pause All`/`Resume All` (desabilitados sem pares), separador, `Hide Window` e `Quit syncfast`. Nada crítico no rodapé.
  - Uma `sidebar_row` por par: ícone `folder`, nome, `status_badge` compacto (só ícone, com o texto como tooltip) e, embaixo, `subheadline` com `Synced <tempo relativo>` ou `Never synced`.
  - Seleção destacada sempre.
- **Detalhe:**
  - `toolbar`: nome (`title2`), `local ↔ remote` em `callout`/`secondary`; à direita, `primary_button` `Sync Now` (desabilitado durante `Syncing` e com gate), `secondary_button` `Pause`/`Resume` e um menu `…` com `Open Local Folder`, `Open Versions Folder`, separador, `Remove Sync…`. As pastas abrem com `xdg-open`.
  - Cartão de status:
    - `status_badge` grande e uma frase. Com gate de revisão: `Review what will change before syncing resumes.` com `primary_button` `Review…`. Com `auth_error`: `Reconnect Account…`, que abre Settings > Accounts. Com `local_missing`: `<local> is missing.`
    - Durante `Syncing`: `progress_bar` determinada (`transfers/total_transfers`; indeterminada se o total for 0) e a linha `callout` `<n> of <total> files · <bytes>/<total bytes> · <speed>/s · <eta> left`, com `subheadline` `Current: <arquivo>`.
  - `segmented` com `Activity`, `Conflicts`, `Filters`.
- **Activity:**
  - `list_row` por rodada (até 50): data e hora, duração, resultado (`status_badge`), `↑ <uploaded>  ↓ <downloaded>  ✕ <deletes>`.
  - Clicar expande (progressive disclosure) e mostra as últimas 200 mensagens do log em monospace com scroll.
  - Vazio: `empty_state` `No activity yet`.
- **Conflicts:**
  - Lista os `*.conflict<N>` locais (varredura com `DirMatcher`, refeita ao abrir a aba e a cada rodada concluída).
  - Cada linha: caminho relativo, "Current" (tamanho e mtime) e "Conflicted copy" (tamanho e mtime).
  - Ações:
    - `Keep Current`: apaga o `.conflictN`.
    - `Keep Conflicted Copy`: move o atual para `versions/<id>/<AAAAMMDD-HHMMSS>/<caminho relativo>` e renomeia o `.conflictN` para o nome original.
    - `Keep Both`: renomeia para `<stem> (conflict <N>)<.ext>`.
  - Regra de nome do `Keep Both`:
    - `.ext` é tudo a partir do último `.` do nome original, exceto quando o único `.` está na posição 0 (dotfile) ou não há ponto; nesses casos `.ext` é vazio.
    - Se o destino existir, acrescentar ` 2`, ` 3`… antes de `.ext`.
  - Nenhuma ação usa alert: todas são reversíveis via versions/ ou pelo arquivo mantido (HIG: sem alert para ação comum desfazível).
  - Depois da ação, `Sync Now`.
  - Vazio: `empty_state` `No conflicts`.
- **Filters:**
  - Duas `switch` com os presets (`System junk files`, `Developer folders`). Cada uma tem um disclosure `Show rules` com a lista em monospace.
  - `text_area` `Custom rules`, uma regra por linha, com `inline_error` por linha inválida.
  - `secondary_button` `Test Filters`: chama `list_files` com e sem filtro na pasta local e mostra `<x> of <y> files excluded` e uma lista das 20 pastas de primeiro nível com mais arquivos excluídos (`<pasta> · <n> files`).
  - `primary_button` `Save` (desabilitado sem mudança ou com erro): grava, roda `apply`, troca gate `too_many_deletes` ou `all_files_changed` por `needs_review_filters_changed` e mostra o aviso `Saved. The next sync will ask you to review the changes.`

**Sheet Review** (HIG: sheet modal com Cancel):

- Título `Review Changes for <name>`. Subtítulo conforme o motivo: `First sync`, `Filters changed`, `Sync stopped after an error`, `Many files would be deleted`, `Every file changed`.
- Ao abrir, roda o preview (`resync` para `needs_review_*`, `force` para `too_many_deletes` e `all_files_changed`) com `progress_bar` indeterminada e `Checking what would change…`.
- Resultado: seções recolhíveis, só as não vazias, com contagem:
  - `Would upload to <remote>`
  - `Would download to this computer`
  - `Would replace on this computer (old version kept for 30 days)`
  - `Would replace on <remote>`
  - `Would delete on this computer`
  - `Would delete on <remote>`
  - `Other changes`
- Nota em `footnote`: `Preview from a dry run. A few listed deletions may not happen in the real sync.`
- Nada a mudar: `Nothing to change.`
- Botões: `Cancel` e `Sync` (primário; `Delete and Sync` quando houver qualquer exclusão, e nesse caso com estilo destrutivo e sem ser default, seguindo o HIG).
- Confirmar grava `confirm.json` com o `kind` e o `filters_hash` do preview, dispara `Sync Now` e fecha.
- Preview ocupado (exit 3): `This sync is running. The review opens when it finishes.` A sheet tenta de novo a cada 5 s.
- Erro do preview: `inline_error` com a última linha ERROR e `Try Again`.

**Sheet Add Sync** (assistente; Back e Continue no rodapé, Cancel à esquerda; nunca Cancel, Back e Done juntos):

1. `Account`: lista de remotes (ícone e nome; aviso `warning` `Uses rclone's shared client ID` quando aplicável) e `Add Google Drive Account…`, que empilha a sheet de conta depois de fechar esta (HIG: uma sheet por vez; o assistente reabre no passo 1 depois).
2. `Folder in <remote>`: `folder_tree` com `list_dirs` sob demanda, a opção `Entire <remote>` no topo e `New Folder…` (campo inline e `rclone mkdir`).
3. `Folder on This Computer`: caminho com `Choose…`, que abre `rfd::AsyncFileDialog::pick_folder` via portal. Caminho inexistente oferece `Create Folder`.
4. `Filters`: a mesma UI da aba Filters, com `Test Filters`.
5. `Name`:
   - Campo com padrão no último segmento da pasta local.
   - Id gerado: minúsculas, não-alfanumérico vira `-`, colapsa `-` repetido, corta em 32, e acrescenta `-2`, `-3`… se colidir.
   - Mostrar `Sync id: <id>` em `footnote`.

- `Create Sync` (primário do último passo): grava, roda `apply`, fecha e abre a sheet Review do novo par.
- Erros de validação do passo 3 aparecem como `inline_error` no passo correspondente; Continue fica desabilitado.

**Boas-vindas** (janela principal, quando não há pares e `welcome_done = false`):

- `large_title` `Welcome to syncfast`.
- `body`: `Keep folders on this computer in sync with Google Drive. Your files stay local first.`
- Três passos numerados com estado (pendente, atual, feito): `Connect your Google account`, `Choose a folder`, `Review the first sync`.
- `primary_button` `Get Started`:
  - Sem remote `drive` → sheet de conta.
  - Depois → sheet Add Sync.
  - Depois → Review.
- `secondary_button` `Set up without the guide`: grava `welcome_done = true` e leva à janela principal vazia.
- No topo à direita, `icon_button` `settings` (`Settings…`) e o mesmo menu `ellipsis` da sidebar, para Configurações e Quit sem passar pela bandeja.
- O passo 3 fica feito quando a primeira rodada `Ok` acontece. Aí `welcome_done = true` e a tela não volta.
- Com `welcome_done = true` e sem pares: `empty_state` `No syncs yet` com `Add Sync…`.

**Tela Settings** (substitui o conteúdo da janela principal, sem segunda janela; título da janela igual ao painel ativo, lembra o último painel; `icon_button` `chevron-left` (`Back`) no topo da sidebar e `Esc` voltam para a janela anterior; abrir Settings com a janela escondida na bandeja mostra a janela principal já em Settings; as sheets de conta abrem por cima dela):

- Sidebar com `Accounts` e `Appearance`.
- **Accounts:**
  - Lista de remotes com tipo.
  - Remote `drive` sem client_id próprio: linha `warning` `Uses rclone's shared client ID, which stops working in 2026.` com `Use My Client ID…`.
  - Par com gate `auth_error` destaca o remote com `Reconnect…`, que roda `update_drive_credentials` com o client_id atual em branco. Se o remote já tem client_id próprio, os campos vêm preenchidos com o id e o secret mascarado.
  - `Add Google Drive Account…` abre a sheet de conta.
- **Sheet de conta:**
  - Campos `Account name` (regex `^[A-Za-z0-9_][A-Za-z0-9_.-]*$`, sem colidir), `Client ID`, `Client secret` (mascarado).
  - Texto: `Create an OAuth client of type "Desktop app" in Google Cloud, enable the Google Drive API, and set the consent screen to "In production" so access does not expire every 7 days.` com link para https://rclone.org/drive/#making-your-own-client-id.
  - `Connect` roda o comando e mostra `Finish signing in in your browser…` com `Cancel`, que mata o filho.
  - Sucesso: fecha e recarrega a lista; limpa gates `auth_error` de pares desse remote e dispara `Sync Now` neles.
  - Falha: `inline_error` com a última linha do stderr.
- **Appearance:** seletor `Theme`: `System`, `Light`, `Dark` e os temas de `themes/` (incluindo `noctalia.json` quando existir). Mostra os erros de tema do catálogo.

**Alert Remove Sync** (HIG: ação destrutiva incomum):

- Título `Remove "<name>"?`.
- Texto: `syncfast will stop syncing this folder. Files on this computer and on <remote> are not deleted.`
- Checkbox `Also delete saved versions`, desmarcado.
- Botões `Cancel` (default) e `Remove` (destrutivo).
- Confirmar remove o par do config e roda `apply` com `--delete-versions` se marcado.

### 14. Testes

- **Unitários (core):**
  - config: cada erro global e por par, `~` expandido, sobreposições, canonicalização
  - `Behavior::default`
  - `render`/`hash`
  - `DirMatcher`: ancorado, não ancorado, `**`, desligado por `+`
  - `bisync_args`: snapshot por modo
  - `classify`: um teste por ramo, com fixtures
  - `summarize` e `last_stats`: com fixtures
  - `decide`: uma linha da tabela por teste, mais o caso pausa e retoma
  - `display_status`: precedência
  - render de units com escapes
  - nomes de conflito: com extensão, sem extensão, dotfile, `a.tar.gz`, colisão
  - poda de versões pelo diretório datado
  - slug do id
- **Integração** (`crates/syncfast-core/tests/bisync_local.rs`):
  - Ignorado com aviso se não houver `rclone` no PATH.
  - Cada teste cria `TempDir` com `local/`, `remote/` e um `rclone.conf` contendo `[testremote]\ntype = local\n`. Exporta `RCLONE_CONFIG` e os `XDG_*` apontando para o tempdir (o teste roda as funções do core com um `Paths` construído explicitamente, sem variável global). O remote do par é `testremote:<tempdir>/remote`.
  - A rede é um `Network` fake e o systemd está em modo `Off`.
  - Cenários:
    1. Primeira rodada → gate `needs_review_first_run`, sem tocar arquivos. Preview `resync` lista upload e download esperados. Marker e `run` → `ok`, e os lados ficam iguais.
    2. Arquivo novo local → `run` → aparece no remoto, `uploaded = 1`.
    3. Mesmo arquivo alterado nos dois lados → vence o mais novo, existe `.conflict1`, `conflicts_new = 1`, `conflicts_pending = 1`.
    4. Arquivo alterado no remoto → versão local antiga em `versions/<id>/<AAAAMMDD-HHMMSS>/`.
    5. Apagar 3 de 4 arquivos locais → gate `too_many_deletes`, remoto intacto. Marker `force` → remoto apagado, `ok`. Pasta com um só arquivo, editado no local → gate `all_files_changed`; marker `force` → remoto atualizado, `ok`.
    6. Mudar `filters` → gate `needs_review_filters_changed`, nada sincronizado.
    7. Pasta local renomeada → gate `local_missing`. Pasta de volta → `run` normal, `ok`. Partindo de `needs_review_first_run`: pasta some e volta, o gate continua e a revisão ainda é pedida.
    8. `needs_review` → `enabled = false` → `run` (Skip) → `enabled = true` → `run` ainda `Skip(Gated)`.
    9. `apply` em modo Off com par removido → arquivos de estado, filtro, `.md5` e workdir apagados; `versions/` mantido.
- **Fronteira:** `boundary.rs` (passo 1).
- **UI** (`crates/syncfast-gui/tests/ui_*.rs`, `egui_kittest::Harness` sobre as funções de tela com `ViewModel` fake; asserções por rótulo e papel de acessibilidade e pelas `Action`s emitidas):
  - Boas-vindas: estados dos 3 passos e `Get Started` emitindo a ação certa com e sem remote.
  - Add Sync: Continue desabilitado com cada erro de validação e mensagem visível; `Create Sync` emite `CreatePair` com o slug esperado.
  - Review: vazio (`Nothing to change.`), com uploads, com exclusões (botão `Delete and Sync`), ocupado.
  - Conflicts: as três ações emitem a `Action` com os caminhos esperados.
  - Accounts: aviso de client_id compartilhado aparece só para remote `drive` sem client_id.
  - Detalhe: `Sync Now` desabilitado durante `Syncing` e com gate.
- **Mutação:** `just mutants`. Meta: zero sobrevivente em `bisync.rs`, `run_decision.rs`, `filters.rs`, `state.rs` (`display_status`). Sobrevivente vira teste ou entra em `.cargo/mutants.toml` `exclude_re` com uma linha de motivo.

### 15. Instalação e migração nesta máquina

Rodar depois de `just check` verde e do roteiro dev da Verification.

1. `just flatpak` (build e `flatpak install --user`, ADR 0006). O Flatpak exporta o desktop entry e o ícone, então o dotfiles não guarda `syncfast.desktop`.
2. Remover o desktop entry antigo `~/.dotfiles/desktop/.local/share/applications/syncfast.desktop` e, quando houver, `~/.local/bin/{syncfast,syncfast-gui}`.
3. Config no dotfiles:
   - Em `~/.dotfiles/install.sh`, `camada_desktop()`, acrescentar `.config/syncfast` à lista de diretórios do `linkar`.
   - Criar `~/.dotfiles/desktop/.config/syncfast/config.toml` com `version = 1`.
   - Rodar `cd ~/.dotfiles && stow -t "$HOME" -R desktop`.
   - Confirmar com `readlink ~/.config/syncfast/config.toml`.
4. Template Noctalia:
   - Copiar `contrib/noctalia/syncfast/` para `~/.local/state/noctalia/community-templates/syncfast/`.
   - Acrescentar `"syncfast"` a `community_ids` em `~/.dotfiles/noctalia/.local/state/noctalia/settings.toml`.
   - Se o Noctalia não aplicar um template que não está no `catalog.json` da comunidade, gerar `~/.config/syncfast/themes/noctalia.json` à mão a partir de `syncfast-dark.json` com as cores atuais e registrar a limitação no `README.md`.
5. Autostart: acrescentar `hl.exec_cmd("flatpak run io.github.raphael_soares.syncfast --start-hidden")` depois de `hl.exec_cmd("noctalia")` em `~/.dotfiles/hypr/.config/hypr/autostart.lua`.
6. Abrir o app pelo launcher (ou `flatpak run io.github.raphael_soares.syncfast`), `Settings… > Accounts > Use My Client ID…` no `gdrive`, concluir o OAuth.
7. Remover o setup antigo:
   - `systemctl --user disable --now rclone-gdrive.service obsidian-sync.timer`
   - `rm ~/.config/systemd/user/rclone-gdrive.service ~/.config/systemd/user/obsidian-sync.service ~/.config/systemd/user/obsidian-sync.timer`
   - `systemctl --user daemon-reload`
   - `rm ~/.config/rclone/gdrive-exclude.txt ~/.config/rclone/obsidian-filter.txt`
   - `rm ~/.cache/rclone/bisync/home_raphael_Notes..gdrive_Notes.*`
   - Se `/mnt/Drive` ainda estiver montado: `fusermount -u /mnt/Drive`. O diretório `/mnt/Drive` não é apagado (exige sudo); fica para o usuário.
8. Quais pastas viram pares é decisão de uso, feita pela GUI depois.

### 16. Interface para agentes de IA (`syncfast-core/src/ops/`, CLI, `syncfast mcp`)

Decisão e limites em `docs/adr/0007-agent-interface.md`; guia em `docs/agents.md` (o mesmo texto que `syncfast agent-guide` imprime).

- **Operações no core:** `ops::Ops` (caminhos, rclone, rede, notificação, systemctl, binário e `Trigger`) com um método por operação, cada um devolvendo structs `Serialize` ou `OpError` (`Invalid`, `NotFound`, `Busy`, `DeletesNeedUser`, `ReviewNotShown`, `Failed`). CLI e MCP só traduzem argumentos e imprimem. `runs.rs` (histórico de rodadas) passou da GUI para o core.
- **O agente faz tudo que o usuário faz, menos autenticar:** nenhuma operação cria conta, troca credencial ou roda OAuth. `accounts` lista nome, tipo e o aviso de client_id compartilhado, nunca segredo.
- **Subcomandos** (todos com `--json`, global; o texto humano é o padrão e a saída atual de `apply`, `run`, `watch`, `preview` e `status` não muda; `watch` não imprime nada, então `--json` não tem efeito nele): `status` (`--json`: tudo de cada sync), `show`, `add`, `remove [--delete-versions]`, `pause`/`resume` (`<id>` ou `--all`), `filters set`, `filters test`, `accounts`, `folders`, `mkdir`, `explore`, `copy <origem> <destino> [--dry-run] [--wait <s>]`, `move` (igual), `transfer <id> [--wait <s>]`, `sync [<id>|--all] [--wait <s>]`, `review show`, `review confirm --filters-hash`, `conflicts list`, `conflicts resolve --keep current|conflicted|both`, `activity`, `log`, `theme [<nome>]`, `mcp`, `agent-guide`. Com `--json`, erro sai no stdout como `{"error": {"code", "message"}}`.
- **`add`:** valida pelo core (mesmas mensagens), exige pasta local existente (`--create-local` cria) e pasta remota existente (`--create-remote` cria), grava o config, roda `apply` e devolve o sync com o gate `needs_review_first_run`.
- **`filters set`:** troca presets e regras; o que não for dado fica. Mesma regra da GUI: gate `too_many_deletes` ou `all_files_changed` vira `needs_review_filters_changed`; com o hash novo e sem gate, uma rodada curta grava o gate de revisão.
- **`explore <local|remote:caminho> [--depth N] [--max-folders N]`:** `rclone lsjson -R` e agregação em memória: por pasta até a profundidade (padrão 2), arquivos, arquivos diretos, subpastas, bytes, extensões mais comuns e data do arquivo mais novo. Só metadados, nunca conteúdo. Saída limitada a `max-folders` (padrão 60, as maiores primeiro, `folders_omitted` diz quantas ficaram de fora). Tamanho desconhecido (Google Docs) conta 0 e aparece em `files_of_unknown_size`.
- **Rodadas:** `sync` e `review confirm` disparam a rodada pelo `Trigger` (systemd, ou filho em `SYNCFAST_SYSTEMD=off`) e esperam até `--wait` segundos (padrão 60; 0 devolve logo). A rodada continua sozinha depois; `finished: false` diz que ainda corre.
- **Revisão pelo agente:** `review show` roda o preview do gate e grava o que mostrou em `pairs/<id>/preview.json` (`kind`, `filters_hash`, `changes`). `review confirm` só aceita quando:
  1. o gate espera revisão (`needs_review_*` pede `resync`; `too_many_deletes` e `all_files_changed` pedem `force`);
  2. existe `preview.json` do mesmo `kind` do gate;
  3. o preview não tem `DeleteLocal` nem `DeleteRemote`;
  4. o `--filters-hash` é o do preview e é o hash atual dos filtros.
  Aceito, grava `confirm.json`, apaga `preview.json` e dispara a rodada. Recusado, nada muda. Exclusão: exit 4 e `This review deletes files. Confirm it in syncfast.`; preview ausente, hash diferente ou filtros mudados: exit 5. Não há flag para contornar.
- **Códigos de saída:** 0 ok, 1 falha, 2 pedido ou config a corrigir (inclui sync inexistente), 3 sync rodando, 4 só o usuário confirma, 5 mostrar a revisão de novo. Os de `run`, `preview`, `status` e `watch` continuam.
- **GUI reflete o agente:** o backend relê `config.toml` e todos os `state.json` a cada ciclo (1, 2 ou 10 s), então par criado, pausado, filtrado ou removido pela CLI aparece sem reiniciar. `syncfast theme` grava `theme` em `gui.json` e manda `reload-themes`; a GUI relê o tema do arquivo ao receber esse pedido. O `watch` já recarrega quando o config muda (passo 9).
- **`syncfast mcp`:** servidor MCP em stdio com `rmcp`. Ferramentas: `status`, `show_sync`, `add_sync`, `remove_sync`, `pause_sync`, `resume_sync`, `set_filters`, `test_filters`, `list_accounts`, `list_remote_folders`, `create_remote_folder`, `explore`, `copy`, `move`, `show_transfer`, `sync_now`, `show_review`, `confirm_review`, `list_conflicts`, `resolve_conflict`, `list_activity`, `show_round_log`, `theme`, cada uma com descrição e JSON Schema. Erro do core vira resultado de ferramenta com `isError` e a mensagem do core. O `initialize` traz `instructions` com o fluxo: explorar, propor, adicionar, revisar, confirmar ou pedir ao usuário.
- **`syncfast agent-guide`:** imprime `docs/agents.md` (embutido no binário): fluxo, comandos, regras de segurança, códigos de saída e como configurar o MCP no Claude Code e no omp, inclusive `flatpak run --command=syncfast io.github.raphael_soares.syncfast mcp`.
- **Testes:** ops com rclone real local (`crates/syncfast-core/tests/ops.rs`: adicionar sync, revisão sem exclusão confirmada, revisão com exclusão recusada com nada alterado, hash velho e filtros mudados recusados, explorar com contagens e tamanhos exatos e sem conteúdo, contas sem segredo, conflitos, filtros, pausar, remover, atividade, tema); CLI e MCP de ponta a ponta com o binário (`crates/syncfast-cli/tests/agent.rs` e `mcp.rs`: initialize, tools/list, tools/call de `status`, `explore` e `confirm_review`).

## Processo de construção

### Repositório e rastreio

- Repositório público `raphael-soares/syncfast` no GitHub. O único CI remoto é `.github/workflows/flatpak.yml`, que empacota e publica o repositório Flatpak no Pages a cada push na `main` (ADR 0006). Os portões de qualidade são o `just check` e o lefthook.
- Criação, dentro da fatia S1, depois do primeiro commit:
  1. `gh auth switch -u raphael-soares`
  2. `gh repo create raphael-soares/syncfast --private --source . --remote origin --push`
  3. `gh auth switch -u soaresphael`, para devolver a conta ativa de antes.
  - Todo `gh` posterior usa `--repo raphael-soares/syncfast` e roda entre os mesmos dois `gh auth switch`.
- Na S1, esta spec é copiada para `docs/design/spec-phase-1.md` e o mock para `docs/design/ui-mock.html`. As issues referenciam os passos da spec por número.
- Milestone `Phase 1` com uma issue por fatia:
  - Título `[S<n>] <título da tabela abaixo>`.
  - Corpo com o objetivo, os passos da spec cobertos e os critérios de aceite da tabela, em checklist.

### Fatias (sequenciais, nesta ordem)

| Fatia | Título | Passos da spec | Critério de aceite |
|---|---|---|---|
| S1 | Bootstrap and rclone fixtures | 1, 2 | Workspace compila com os 3 crates vazios. `just check` verde, `boundary.rs` passa, `lefthook` instalado e rejeitando commit fora do padrão. Fixtures dos 11 cenários commitados. ADRs 0001 a 0004, `AGENTS.md`, `README.md`, spec e mock em `docs/`. |
| S2 | Config, behavior and filters | 3, 4 | Testes unitários de config (cada erro global e por par), `Behavior::default`, `render`/`hash`, `DirMatcher`. |
| S3 | Bisync contract and state | 5, 6 | Snapshot de `bisync_args` por modo; `classify`, `summarize`, `last_stats` testados contra os fixtures; `display_status` com a precedência completa. |
| S4 | Run decision and `syncfast run` | 7 | `decide` com um teste por linha da tabela. Cenários de integração 1 a 8 passando. Notificação com fallback sem GUI. |
| S5 | Units, apply, watch, preview, status | 8, 9, 10 | Render de units com escapes; cenário de integração 9; `watch` disparando em modo `SYNCFAST_SYSTEMD=off`. **Marco:** com `just run-cli-dev`, um par `devremote` sincroniza de ponta a ponta sem GUI, e `syncfast status` mostra o estado. |
| S6 | GUI shell, design system and tray | 11, 12 | Janela abre e esconde; bandeja com menu e ícone por estado; instância única com `show`, `notify`, `reload-themes`; tema claro/escuro e tema de usuário; template Noctalia em `contrib/`. |
| S7 | Main window, activity and progress | 13 (janela principal, Activity) | Sidebar, detalhe, cartão de status com progresso ao vivo, aba Activity. Testes de UI do detalhe. Bate com os quadros 2 do mock. |
| S8 | Review, Add Sync and Welcome | 13 (Review, Add Sync, Boas-vindas) | Fluxo completo no modo dev: boas-vindas → conta → Add Sync → Review → Up to date. Testes de UI de boas-vindas, Add Sync e Review. Bate com os quadros 1, 3 e 4 do mock. |
| S9 | Settings, conflicts, filters and removal | 13 (Settings, Conflicts, Filters, Remove) | Accounts (criar, trocar credencial, aviso), Appearance, aba Conflicts (3 ações), aba Filters com Test Filters, alert de remoção. Testes de UI correspondentes. `just mutants` sem sobrevivente nos módulos-alvo. Bate com os quadros 5 e 6 do mock. |
| S10 | Install, migration and Drive end-to-end | 14, 15, Verification 3 e 4 | Instalado como Flatpak, setup antigo removido, roteiros dev e Drive real da Verification executados e anotados na PR. **Marco final.** |

### Fluxo de cada fatia

1. Branch `s<n>-<slug-do-título>` a partir da `main` atualizada, no próprio checkout (fatias sequenciais, sem worktree).
2. Implementar só o escopo da fatia. Commits pequenos no padrão Conventional Commits, sem `Co-Authored-By`, sem menção a IA, sem emoji.
3. `just check` verde. Nas fatias de GUI, também rodar `just run-gui-dev` e conferir a tela contra o mock.
4. Revisão por agente independente (`reviewer`), com estas entradas: a issue da fatia, os passos da spec cobertos e o diff. Checklist:
   - critério de aceite cumprido por inteiro
   - comportamento igual à spec, inclusive textos de UI
   - testes de comportamento (skill `testes`)
   - comentários (skill `comentarios`)
   - sem código morto, sem `unwrap` fora de teste, sem cor literal fora de `theme.rs`
   - fronteira de crates respeitada
   - Achados são corrigidos e a revisão roda de novo, até sair limpa.
5. PR `--repo raphael-soares/syncfast` com título no formato Conventional Commits, corpo com `Closes #<issue>`, o resumo e a saída final do `just check`.
6. Fatias comuns: o agente faz o merge (squash), apaga o branch e segue para a próxima.
7. S5 e S10: o agente abre a PR e **para**, esperando o usuário olhar e fazer o merge.

## Critical files & anchors

- `crates/syncfast-core/src/run_decision.rs` `decide`: a tabela do passo 7, onde a revisão impede resync e `--force` sem confirmação.
- `crates/syncfast-core/src/bisync.rs` `bisync_args` e `classify`: contrato com o rclone, travado pelos fixtures de `tests/fixtures/rclone-1.75.2/`.
- `crates/syncfast-core/src/state.rs` `display_status`: precedência única para sidebar, bandeja e status da CLI.
- zapfast `src/main.rs`, `src/app.rs`, `src/backend.rs`, `src/single_instance.rs` e spotifast `src/theme.rs`, `src/ui/widgets.rs`, `src/ui/sidebar.rs` (https://github.com/crmne): modelos de casca, backend, instância, paleta, estilo e componentes.
- `~/.dotfiles/install.sh` `camada_desktop()` e `~/.dotfiles/hypr/.config/hypr/autostart.lua`: integração com a sessão.

## Verification

Diretório `~/Desktop/Personal/syncfast`.

1. `just check` verde, com os 9 cenários de integração e os testes de UI.
2. `just mutants` sem sobrevivente nos quatro módulos-alvo.
3. Roteiro dev (sem Drive e sem systemd):
   1. `just dev-reset && just run-gui-dev`: aparece a boas-vindas.
   2. `Get Started` → conta `devremote` → pasta remota `Entire devremote` → pasta local `./.dev/local` (criar) → nome `dev` → `Create Sync`. Abre a Review com `Nothing to change.`
   3. `Sync`: o status vai a `Up to date`.
   4. `echo a > .dev/local/a.txt`. Em até 45 s, `.dev/remote/a.txt` existe; Activity mostra `↑ 1`.
   5. `echo b > .dev/remote/b.txt` e `Ctrl+R`: `.dev/local/b.txt` aparece.
   6. Aba Filters: regra `- b.txt` e `Save`. O status vira `Review needed`; `Review…` mostra a sheet; `Sync` volta a `Up to date`.
   7. Apagar `a.txt` e criar `c.txt`, `d.txt`, `e.txt` locais, `Ctrl+R`, depois apagar `c.txt` `d.txt` `e.txt`, `Ctrl+R`. Gate `Many files would be deleted`; a Review mostra `Would delete on devremote (3)` e o botão `Delete and Sync`.
4. Drive real, depois da migração:
   1. `rclone mkdir gdrive:syncfast-e2e && mkdir -p ~/syncfast-e2e && echo a > ~/syncfast-e2e/a.txt`.
   2. Add Sync com `gdrive` / `syncfast-e2e` / `~/syncfast-e2e`. A Review mostra `Would upload to gdrive (1)`. `Sync`.
   3. `rclone lsf gdrive:syncfast-e2e` lista `a.txt`.
   4. `syncfast status` imprime `syncfast-e2e	UpToDate	<data>`.
   5. `systemctl --user list-timers 'syncfast-*'` lista o timer.
   6. `echo b > ~/syncfast-e2e/b.txt`. Em até 45 s, `rclone lsf gdrive:syncfast-e2e` lista `b.txt`, e `journalctl --user -u syncfast-syncfast-e2e.service -n 5` mostra a rodada.
   7. `echo c | rclone rcat gdrive:syncfast-e2e/c.txt`. Em até 5 min 30 s, `~/syncfast-e2e/c.txt` existe.
   8. `nmcli networking off && systemctl --user start syncfast-syncfast-e2e.service && syncfast status`: mostra `Offline`, sem notificação. Depois `nmcli networking on`.
   9. Copiar um arquivo grande (>200 MB) para a pasta: o cartão de status mostra barra determinada com velocidade e ETA.
   10. Depois de relogar, `pgrep -a syncfast-gui` mostra o processo e o ícone está na bandeja do Noctalia.
   11. Limpeza: `Remove Sync…` com `Also delete saved versions`, `rm -r ~/syncfast-e2e`, `rclone purge gdrive:syncfast-e2e`.

## Assumptions & contingencies

- **Patch do egui/winit:** se o `[patch.crates-io]` não resolver junto da tag v0.15.6 do fastframe, remover o bloco inteiro e registrar no `AGENTS.md`.
- **Bandeja invisível:** se a bandeja não aparecer no Noctalia (`Tray::is_shown()` falso 10 s depois de iniciar), `--start-hidden` passa a abrir a janela minimizável, e fechar a janela minimiza em vez de esconder. Registrar no `AGENTS.md`.
- **PATH do Hyprland:** se `hl.exec_cmd("flatpak run io.github.raphael_soares.syncfast --start-hidden")` não subir no login por PATH, usar `/usr/bin/flatpak run ...`.
- **OAuth parado:** se `rclone config create` com stdin nulo parar sem abrir o navegador, acrescentar `--non-interactive` e tratar o JSON de pergunta devolvido: responder `config_is_local=true` via `--continue --state <state> --result true` até o fim.
- **Lucide:** se algum ícone citado não existir no set Lucide do `fastframe-icons`, entra como SVG próprio copiado do Lucide (ISC), sem trocar o ícone.
