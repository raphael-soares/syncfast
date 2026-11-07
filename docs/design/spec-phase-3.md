# syncfast: spec da fase 3

## Contexto

Sync não é backup: ele copia exclusões e edições, e o que o `bisync` guarda (versions locais por 30 dias, lixeira e revisões do Drive) não é um histórico que o usuário controle. A fase 3 acrescenta backup periódico, versionado e criptografado de qualquer lista de pastas, para um remote do rclone (Google Drive) ou para uma pasta local (disco externo).

Roadmap:

- **Fase 1:** o app de sync (`docs/design/spec-phase-1.md`).
- **Fase 2:** opções configuráveis e modos de sync (`docs/design/spec-phase-2.md`). Esta spec usa dela: o `version` do config, as tabelas `[notifications]` e `[app]`, o mecanismo de item só do usuário (`lowers_safety`), o tipo de velocidade de `limit_upload` e `limit_download` e a lista de painéis do Settings.
- **Fase 3 (esta):** backup com restic.

Estado final da fase 3:

- `restic` empacotado no Flatpak, ao lado do rclone, com versão e sha256 fixados (ADR 0009).
- `[[backup]]` no config: N backups, cada um com pastas, destino, agenda, retenção, filtros e limites de banda.
- Chave do repositório gerada pelo syncfast e guardada no keyring: pelo portal Secret quando existe, senão pelo Secret Service do host; sem nenhum dos dois, nenhum backup é criado. Mostrada uma vez ao criar.
- Backup diário por timer systemd com `Persistent=true`, retenção aplicada com `forget --prune`, verificação de integridade periódica, falhas na gate `needs_attention`.
- Lista de snapshots, navegação pelos arquivos de um snapshot e `Restore To…` numa pasta nova, nunca no lugar.
- Agentes listam, criam e editam backups (sem baixar a segurança), rodam, verificam e restauram para pasta nova; nunca veem a chave.
- Duas máquinas podem usar o mesmo repositório: cada uma usa o hostname real como `--host` e a retenção de uma só olha os snapshots dela.

## Decisões fechadas

### Do dono

- **Motor:** restic, empacotado no Flatpak como o rclone (binário de release pré-compilado, sha256 fixado), com o rclone como backend (`rclone:<remote>:<caminho>`), o que reaproveita as contas. Destino também pode ser um caminho local (disco externo). Por que restic e não kopia nem só rclone: ADR 0009.
- **Destino fora dos syncs:** o destino fica fora da pasta remota e da pasta local de todo sync, e a config valida isso (passo 2).
- **Origem:** qualquer lista de pastas. Presets e filtros do sync são reaproveitados e mapeados para `--exclude-file` do restic, respeitando as diferenças de sintaxe (passo 5).
- **Config:** `[[backup]]` com `id`, `name`, `paths`, `dest`, `every`, `keep`, `presets`, `filters`, `enabled`, `one_file_system`, `limit_upload` e `limit_download`.
- **Chave:**
  - O syncfast gera a senha do repositório e a guarda, nesta ordem:
    1. no keyring pelo portal XDG Secret (`org.freedesktop.portal.Secret`, crate `oo7`);
    2. sem o portal, no Secret Service do host (`org.freedesktop.secrets`, também pelo `oo7`), com `--talk-name=org.freedesktop.secrets` nos `finish-args` do Flatpak;
    3. sem nenhum dos dois, nenhum backup é criado.
  - O portal continua o preferido: o ADR 0009 registra o custo do plano B (esse `talk-name` deixa o sandbox alcançar todos os segredos do keyring do host).
  - Ela aparece uma vez, com o passo obrigatório `I Saved the Key`, e pode ser mostrada de novo depois, atrás de uma confirmação.
  - Nunca é logada.
  - Restaurar em outra máquina é digitar a chave.
- **Agenda:** diária por padrão, com timer systemd `Persistent=true`. Retenção padrão `keep = { daily = 7, weekly = 4, monthly = 6 }`, com `yearly` opcional (padrão 0, desligado), aplicada com `restic forget --prune` depois do backup. Configurável por backup. `last` e `hourly` não estão previstos.
- **Integridade:**
  - `restic check --read-data-subset=5%` todo mês.
  - Depois de um backup que falhou, um `restic check` estrutural, sem `--read-data` (índices, árvores e snapshots, sem ler os packs): `CheckKind::Quick`.
  - Falhas alimentam a gate `needs_attention` existente, estendida a backups.
  - Locks velhos do restic são tratados (passo 8).
- **Restore:** lista de snapshots, árvore de arquivos do snapshot (`restic ls --json`) e `Restore To…` numa pasta que o usuário escolhe, nunca no lugar (`restic restore --target --include`).
- **Produto:** o `PRODUCT.md` deixa de dizer "Not a backup tool" (o pai edita). ADR 0009 registra restic contra kopia e rclone puro, o modelo de chave e as regras de destino.
- **Agentes:** podem listar backups e snapshots, rodar um backup agora e **criar e editar backups** sob a guarda `lowers_safety` da fase 2; restore só para pasta nova, nunca no lugar. Só o usuário (exit 4): reduzir qualquer valor de `keep`, remover um backup, remover uma pasta de origem, mudar `dest` e desligar `one_file_system`. A chave nunca chega a um agente. Um backup criado por agente gera e guarda a chave e já roda, e a GUI mostra o aviso `Save the backup key` até o usuário concluir `I Saved the Key` (passo 15).
- **Duas máquinas por repositório:** o `--host` é o hostname real (o padrão do restic), não um nome fixo. Cada máquina tem o seu `[[backup]]` no mesmo repositório; a retenção agrupa por host e pastas (padrão do restic) e cada máquina só apaga os próprios snapshots. Restaurar numa máquina nova é digitar a chave (`Add Existing Backup…`).
- **Artefatos de build:** preset novo `build_artifacts` em `filters.rs`, ao lado dos outros e usável pelos syncs, ligado por padrão em backup novo; `dev_dirs` não é padrão de backup (passo 5).
- **Banda e arquivos:** `limit_upload` e `limit_download` por backup (restic `--limit-upload` e `--limit-download`), sem limite por padrão; `one_file_system` por backup, padrão `true`, que se desliga para incluir subvolumes Btrfs.
- **Fechado sem trabalho nesta fase:** a assinatura do restic não é verificada além do sha256 fixado, atualizado por uma pessoa num PR; o reparo de repositório é de uma fase futura; o mock das telas de backup é a primeira atividade da fatia P3.2 (issue #57).

### Da spec (derivadas das decisões acima; o dono revisa)

- **Um repositório por backup do config.** `dest` é de um backup só deste config; dois backups não compartilham destino, nem um dentro do outro. Outra máquina pode abrir o mesmo repositório com o próprio `[[backup]]`, cada uma com o seu hostname.
- **Host = hostname real.** `backup` e `forget` recebem `--host <hostname>` (o padrão do restic), lido uma vez por `backup_host()`. O snapshot pai é o último com o mesmo host e as mesmas pastas (agrupamento `host,paths`, o padrão do restic 0.19.1; ver Fatos). A retenção usa `--host <hostname> --group-by host`: só olha snapshots desta máquina, e todos eles formam um grupo só, qualquer que seja a lista de pastas. Assim mudar as pastas de um backup não deixa um grupo antigo que nunca envelhece: os snapshots da lista antiga saem pela mesma política. Renomear a máquina ainda deixa os snapshots do hostname antigo como de "outra máquina" (listados, nunca apagados), e a primeira rodada com o nome novo relê tudo, mas o envio é pouco porque o restic deduplica.
- **Sem revisão.** Nenhum nível de `review` da fase 2 vale para backup: backup nunca apaga arquivo do usuário. O que substitui a revisão: as guardas de origem (pasta ausente ou vazia nunca vira snapshot), o alerta ao reduzir a retenção e a gate que segura um repositório danificado.
- **`--one-file-system` por padrão.** `one_file_system = true`: um mount dentro de uma pasta (a pasta do rclone mount, um disco externo) não entra no backup, e o restic também deixa de fora subvolumes Btrfs (a documentação do 0.19.1 diz "don't cross filesystem boundaries and subvolumes"). Para incluí-los, a pasta entra como origem própria ou o backup desliga `one_file_system`. Desligar traz também tudo o que estiver montado dentro das pastas (um rclone mount seria lido inteiro); a GUI avisa, e um agente não consegue desligar.
- **Origem ausente ou vazia nunca vira snapshot.** O restic, com um caminho que não existe, avisa (`does not exist, skipping`), sai com código 3 e grava mesmo assim um snapshot sem aquela pasta (verificado, ver Fatos). O syncfast checa antes de rodar.
- **Filtros de backup só excluem.** Linhas `+ ` do rclone não têm equivalente seguro no restic e são recusadas (passo 5).
- **Um backup sem pasta pode existir desabilitado.** É assim que `Add Existing Backup…` (outra máquina, só para restaurar) entra no config: `enabled = false` e `paths = []`.
- **Edição por agente, com guarda.** Agentes criam e editam backups (passo 15) pelo registro `lowers_safety` da fase 2, estendido a backup (passo 2). Só o usuário: reduzir qualquer `keep`, remover o backup, remover uma pasta de origem, mudar `dest` e desligar `one_file_system` (exit 4, nada é gravado). Pausar e retomar valem para agentes. A chave nunca é exposta: criar por agente gera, guarda e usa a chave dentro do processo, e o backup fica com o aviso `Save the backup key` até o usuário concluir `I Saved the Key` na GUI.
- **Senha por stdin.** O restic lê a senha de `--password-file /dev/stdin` (verificado); a senha nunca vai em argv, em variável de ambiente nem em arquivo.

## Fatos verificados

Verificados nesta máquina com restic 0.19.1 (binário de release, sha256 conferido) e rclone 1.75.2, com remote `type = local` (os mesmos de qualquer teste de integração), e na documentação do restic 0.19.1 (`doc/030_preparing_a_new_repo.rst`, `040_backup.rst`, `045_working_with_repos.rst`, `060_forget.rst`, `075_scripting.rst`).

- **Release.** `restic` 0.19.1, publicado em 2026-07-05. Os assets de Linux são o binário comprimido com bzip2, não um tar:
  - `restic_0.19.1_linux_amd64.bz2`, sha256 `f415415624dcc452f2a02b8c33641791a8c6d6d3b65bbb3543fcf9a25151585c`
  - `restic_0.19.1_linux_arm64.bz2`, sha256 `a5f64aaab53d51e311fa3829124c5b703f2d14cf187d8640b6be3b2b49376465`
  - URL base `https://github.com/restic/restic/releases/download/v0.19.1/`. Os dois hashes batem com o `SHA256SUMS` do release e com o `digest` que a API do GitHub mostra.
- **Backend rclone.** Formato `rclone:<remote>:<caminho>`; o restic inicia e para o rclone (`rclone serve restic --stdio --b2-hard-delete`). Opções `-o rclone.program=<caminho>` (o restic se recusa a rodar um `rclone` achado no diretório atual, por isso o caminho é sempre explícito), `-o rclone.args=<args>` e `-o rclone.timeout=<tempo>` (padrão 1m). `init` num caminho que não existe cria a pasta.
- **`--json`.**
  - `backup` emite JSON lines. O resultado traz `message_type = "summary"` com `files_new`, `files_changed`, `files_unmodified`, `data_added`, `data_added_packed`, `total_files_processed`, `total_bytes_processed`, `snapshot_id`. Linhas `status` (`percent_done`, `files_done`, `total_files`, `bytes_done`, `total_bytes`, `seconds_remaining`, `current_files`) só vêm em execuções longas: **o formato exato sai dos fixtures** (passo 1).
  - `snapshots --json` é um array; cada snapshot tem `id`, `time`, `paths`, `hostname`, `program_version` e `summary` (a mesma estatística do `backup`).
  - `ls --json <snapshot> <pasta>` imprime uma linha `snapshot` e uma linha `node` por entrada (`name`, `type`, `path`, `size`, `mtime`, `permissions`); sem `--recursive` lista só a pasta e ela mesma.
  - `forget --json` imprime um array único (grupos com `keep` e `remove`); a linha de resumo do `prune` **não** apareceu com `--prune --json`, então o syncfast não depende dela.
  - `check --json` imprime `{"message_type":"summary","num_errors":N,"broken_packs":[...]|null,...}` no stdout e erros no stderr; com dano sai 1.
  - Erros fatais terminam com `{"message_type":"exit_error","code":N,"message":...}` no stderr.
- **Códigos de saída.** 0 ok, 1 falha, 3 `backup` não leu algum dado (o snapshot é gravado), 10 repositório não existe, 11 falha ao obter o lock, 12 senha errada, 130 cancelado. Verificados: 3, 10, 11, 12. Código desconhecido é falha.
- **Origem ausente.** `backup` de duas pastas, uma inexistente, sai 3 e grava um snapshot só com a outra. Por isso a guarda do passo 6.
- **Excludes.**
  - `--exclude-file` expande variáveis de ambiente: `$` precisa ser `$$` (`~$$*`, `$$RECYCLE.BIN`). Sem isso `~$*` e `$RECYCLE.BIN` não excluem nada (verificado). Excludes passados em `--exclude` na linha de comando não expandem.
  - Padrão sem `/` inicial casa em qualquer profundidade e por componente de caminho (`a/b` casa `/t/a/b` e `/t/x/a/b`); `/` inicial ancora na raiz do sistema de arquivos, não na pasta do backup; `/` final é ignorado (casa arquivo e pasta); `**` atravessa `/`; `\[` escapa; `!padrão` desfaz uma exclusão anterior.
  - `--dry-run -vv --json` **não** emite `excluded_item` (verificado): a contagem de excluídos vem da diferença de `total_files_processed` entre duas execuções.
- **Senha.** `--password-file /dev/stdin` funciona com pipe (verificado), inclusive senha errada (saída 12).
- **Locks.** Um `backup` morto com `kill -9` deixou o lock no repositório; `forget` logo depois saiu 11 (`repository is already locked by PID ...`); `restic unlock` (sem `--remove-all`) removeu 1 lock. Pelo código do restic 0.19.1 (`internal/restic/lock.go`, `Lock.Stale`), um lock é velho se o timestamp passa de 30 minutos, ou se foi criado nesta máquina (mesmo hostname) e o processo não existe mais; lock de outro hostname nunca é considerado velho antes dos 30 minutos.
- **Restore.**
  - `restore <id> --target T --include /abs/arquivo` recria o caminho absoluto sob `T`.
  - `restore <id>:/pasta --target T --include /sub` restaura relativo a `/pasta` (`T/sub/...`).
  - `--include` sem correspondência sai 0 com `total_files = 0`: o syncfast trata zero arquivos como erro.
  - `--overwrite never` não toca arquivo existente.
  - `restore --dry-run --json` imprime o mesmo resumo sem criar o `--target` (verificado).
  - O resumo vem como `{"message_type":"summary","total_files":..,"files_restored":..,"total_bytes":..,"bytes_restored":..}` (`files_restored` conta pastas também).
- **Portal Secret nesta máquina.** O gnome-keyring está instalado e ativo (`org.freedesktop.secrets`) e `org.freedesktop.impl.portal.Secret` é ativável, mas o frontend do portal **não** expõe `org.freedesktop.portal.Secret`: `gnome-keyring.portal` tem `UseIn=gnome` e o Hyprland roteia com `default=hyprland;gtk`. O arquivo do sistema (`/usr/share/xdg-desktop-portal/hyprland-portals.conf`) só tem essa linha; o do usuário (`~/.config/xdg-desktop-portal/hyprland-portals.conf`, um link para o dotfiles) repete a linha e acrescenta `org.freedesktop.impl.portal.FileChooser=gtk`, sem nenhuma para o Secret. Sem uma linha explícita para `org.freedesktop.impl.portal.Secret`, o portal não existe para o Flatpak, e é aí que entra o Secret Service do host (passo 3). A correção do portal está no passo 17.
- **`oo7`.** Versões: 0.6.0 (2026-02-21) e 0.7.0-beta (2026-09-21), as duas MIT; dependem de `ashpd ^0.13` e de `zbus` 5 (`^5.12` na 0.6.0, `^5.19` na 0.7.0-beta). Verificado no código das duas (crates baixados):
  - Fora de sandbox, `Keyring::new()` usa o Secret Service do host (módulo `dbus`, coleção padrão).
  - Em sandbox, o backend `file` é um arquivo cifrado com o segredo que o portal entrega a cada app. A documentação do `oo7` diz que o sandbox deve preferi-lo, porque ele não expõe os segredos do app a quem fala com `org.freedesktop.secrets`.
  - Sem portal, só a 0.7.0-beta cai para o host: seu `Keyring::new()` tenta `Keyring::sandboxed()` e, no erro `PortalNotFound`, usa `Keyring::host()`. Na 0.6.0, o `new()` propaga com `?` o erro de `ashpd::desktop::secret::retrieve()` antes do `match` que deveria cair para o host (o ramo de `PortalNotFound` só olha o erro de `UnlockedKeyring::load`, que recebe o segredo já obtido), então a queda nunca acontece.
  - Qualquer outro erro do portal (backend que falha, pedido negado) não cai para o host: volta como erro.
  - `Keyring::sandboxed()` e `Keyring::host()` são públicos só na 0.7.0-beta; na 0.6.0 os dois backends se montam à mão (`oo7::file` com `ashpd::desktop::secret::retrieve`, e `oo7::dbus::Service`).
- **restic 0.19.1: agrupamento, banda e `--one-file-system`.** Verificados no código (`cmd/restic/cmd_forget.go`: `--group-by` padrão `Host: true, Path: true`) e na documentação (`060_forget.rst`, `040_backup.rst`, `manual_rest.rst`), não por execução:
  - `forget` agrupa por host e pastas por padrão (`-g, --group-by`), aplica a política a cada grupo e aceita `--host` para só considerar um hostname; `--keep-yearly <n>` existe ao lado de `--keep-daily`, `--keep-weekly` e `--keep-monthly`; as opções de calendário só contam períodos que têm snapshot.
  - O snapshot pai do `backup` é o último do grupo `host,paths` (padrão). A documentação sugere o mesmo `--group-by` no `forget`; o syncfast usa `host` no `forget` de propósito (Da spec, Host = hostname real). O pai só acelera a leitura: com uma lista de pastas nova não há pai e o restic lê tudo uma vez.
  - `--limit-upload` e `--limit-download` são opções globais, em KiB/s, padrão sem limite. Medido nesta máquina com o backend rclone e remote `type = local`: ao contrário do `--bwlimit` do rclone (que deixa passar os primeiros 4 MiB na velocidade cheia, medido na fase 2), o limitador do restic não tem rajada inicial perceptível: 12 MiB com `--limit-upload 1024` levaram 11,9 s, 2 MiB com `512` levaram 4,2 s, e 12 MiB sem limite, 1,0 s.
  - `-x, --one-file-system`: "exclude other file systems, don't cross filesystem boundaries and subvolumes".

## Approach

Os passos estão em ordem de dependência. Cada um termina com `just check` verde. Passos 1 a 9 são do `syncfast-core` (e da CLI interna); 10 a 14 da GUI; 15 é a interface para agentes; 16 são os testes; 17 é docs e instalação. As fatias do processo (seção seguinte) agrupam os passos.

### 1. Restic empacotado e fixtures

- Constante `RESTIC_VERSION = "0.19.1"` em `crates/syncfast-core/src/restic.rs` e `RESTIC_MIN_VERSION = "0.17.1"` (códigos 10 e 11 desde 0.17.0, 12 desde 0.17.1). Fora do Flatpak (modo dev) o `restic version --json` do PATH precisa ser `>= RESTIC_MIN_VERSION`; diferente da fixada só gera um aviso no log.
- Módulo no manifesto `packaging/flatpak/io.github.raphael_soares.syncfast.yml`, antes do módulo `syncfast`:

  ```yaml
  - name: restic
    buildsystem: simple
    build-commands:
      - bzip2 -dc restic.bz2 > restic
      - install -Dm755 restic /app/bin/restic
    sources:
      - type: file
        only-arches: [x86_64]
        url: https://github.com/restic/restic/releases/download/v0.19.1/restic_0.19.1_linux_amd64.bz2
        sha256: f415415624dcc452f2a02b8c33641791a8c6d6d3b65bbb3543fcf9a25151585c
        dest-filename: restic.bz2
      - type: file
        only-arches: [aarch64]
        url: https://github.com/restic/restic/releases/download/v0.19.1/restic_0.19.1_linux_arm64.bz2
        sha256: a5f64aaab53d51e311fa3829124c5b703f2d14cf187d8640b6be3b2b49376465
        dest-filename: restic.bz2
  ```

  O asset é um `.bz2` de um arquivo só, então `type: archive` não serve; `bzip2` vem no SDK (confirmar no primeiro build).
- `just pin-restic <versão>`: roda `scripts/pin-restic.sh`, que baixa o `SHA256SUMS` do release, troca versão e os dois sha256 no manifesto e a constante em `restic.rs`. A assinatura GPG do `SHA256SUMS.asc` não é verificada: a proteção é o sha256 fixado no manifesto, e quem o atualiza é uma pessoa, num PR em que o diff mostra versão, URLs e hashes (decisão do dono).
- `finish-args`: acrescentar `--filesystem=/run/media` e `--filesystem=/mnt`, para disco externo como destino ou origem (a `home` não os cobre), e `--talk-name=org.freedesktop.secrets`, o plano B do armazenamento de chave quando não há portal Secret (passo 3; o custo está no ADR 0009). Confirmar que o `flatpak-builder` aceita os dois `--filesystem`; se não, o destino local fica restrito à `home` e a validação do passo 4 diz isso (Questões em aberto 2). ADR 0006 ganha uma linha para o binário e outra para as permissões.
- Fixtures reais antes do parser, como no passo 2 da fase 1: `crates/syncfast-core/tests/capture_restic_fixtures.sh` (bash, `set -euo pipefail`) cria um tempdir, um `RCLONE_CONFIG` com `[testremote]\ntype = local` e roda o restic fixado, gravando em `crates/syncfast-core/tests/fixtures/restic-0.19.1/<cenário>.{out,err,exit}`. `just capture-restic-fixtures` chama o script.

  | Cenário | O que faz |
  |---|---|
  | `init` | `init --json` via `rclone:testremote:` |
  | `backup_ok` | duas pastas pequenas, `--json` |
  | `backup_status` | 1,5 GiB aleatórios com `RESTIC_PROGRESS_FPS=1`, para pegar linhas `status` |
  | `backup_unreadable` | arquivo sem permissão: saída 3 e snapshot gravado |
  | `backup_missing_path` | uma das pastas não existe: saída 3 e snapshot parcial |
  | `backup_excludes` | `--exclude-file` com `$$`, e o mesmo sem escape |
  | `backup_dry_run` | `--dry-run -vv --json` com e sem excludes |
  | `snapshots` | `snapshots --json` com 3 snapshots |
  | `ls_dir`, `ls_recursive` | `ls --json <id> <pasta>` com e sem `--recursive` |
  | `forget_prune` | `forget --host <host> --group-by host --keep-* --prune --json` removendo 2, inclusive um snapshot de uma lista de pastas anterior |
  | `forget_two_hosts` | dois hosts no mesmo repositório (`backup --host a` e `--host b`): `forget --host a` só remove snapshots de `a` |
  | `check_ok`, `check_sample` | `check --json` e `check --read-data-subset=5% --json` |
  | `check_damaged` | um pack corrompido: saída 1 e `broken_packs` |
  | `restore_include`, `restore_subfolder`, `restore_no_match` | as três formas do restore |
  | `wrong_password`, `no_repo` | saídas 12 e 10 |
  | `locked`, `unlock` | lock deixado por `kill -9`, saída 11, e `unlock` |

  Os parsers dos passos 4, 6, 8 e 9 são escritos contra esses arquivos, nunca contra texto suposto.
- `tests/restic_pin.rs`: a versão da constante aparece no manifesto, as duas URLs seguem o padrão `restic_<versão>_linux_<arch>.bz2` e os dois `sha256` têm 64 hexadecimais.

### 2. Config e estado dos backups

Arquivos: `config.rs` (tabela `[[backup]]`), `backup/config.rs` (tipos e validação), `backup/state.rs`, `paths.rs`.

- Schema (`deny_unknown_fields`; `TOP_LEVEL_KEYS` ganha `backup`):

  ```toml
  version = 3

  [[backup]]
  id = "documents"
  name = "Documents"
  paths = ["~/Documents", "~/Pictures"]
  dest = "gdrive:Backups/documents"
  every = "daily"
  keep = { daily = 7, weekly = 4, monthly = 6, yearly = 0 }
  presets = ["system_junk", "build_artifacts"]
  filters = []
  one_file_system = true
  limit_upload = "off"
  limit_download = "off"
  enabled = true
  ```

  | Chave | Padrão | Valores |
  |---|---|---|
  | `id`, `name`, `paths`, `dest` | obrigatórios | ver validações |
  | `every` | `"daily"` | `hourly`, `daily`, `weekly` |
  | `keep` | `{ daily = 7, weekly = 4, monthly = 6, yearly = 0 }` | cada valor de 0 a 999 (`yearly` é opcional e 0 é desligado); ao menos um maior que 0 |
  | `presets` | `["system_junk", "build_artifacts"]` | `system_junk`, `dev_dirs`, `build_artifacts`, `git_repos` |
  | `filters` | `[]` | linhas `- <padrão>` |
  | `one_file_system` | `true` | `true`, `false` |
  | `limit_upload`, `limit_download` | `"off"` | `off` ou velocidade, o tipo da fase 2: inteiro positivo seguido de `K`, `M` ou `G` (KiB, MiB, GiB por segundo); viram `--limit-upload` e `--limit-download` do restic, em KiB/s (passo 4) |
  | `enabled` | `true` | `true`, `false` |

  Os padrões são constantes (`BackupBehavior::default()`), não herdam da `[defaults]` da fase 2, que descreve o comportamento de sync. `limit_upload` e `limit_download` reutilizam o tipo e a validação de velocidade da fase 2 (as mesmas mensagens), mas só valem para o backup: não herdam o limite do sync. `last` e `hourly` não existem em `keep`.
- `version`: a fase 2 usa 2 e grava sempre `version = 2` (`Config::save`); `[[backup]]` exige 3. Esta fatia muda o contrato da fase 2: `Config::parse` aceita 1, 2 ou 3, e `Config::save` grava 3 quando há `[[backup]]` ou o arquivo já era 3, e 2 nos demais casos (só o inteiro é editado, comentários e ordem ficam). O teste `version_3_is_unsupported` da fase 2 vira `version_4_is_unsupported`. A GUI grava `version = 3` na mesma escrita que acrescenta o primeiro `[[backup]]`. Config com `[[backup]]` e `version < 3` dá erro global `backups need config version 3`. Um syncfast de fase 2 lendo `version = 3` responde `unsupported config version 3`.
- Erros por backup (`BackupError { backup_id, message }`, no mesmo molde de `PairError`; o backup inválido fica sem units, os demais seguem):

  | Condição | Mensagem |
  |---|---|
  | `id` fora de `^[a-z0-9][a-z0-9-]{0,31}$` | `backup id "<id>" must use a-z, 0-9 and "-", up to 32 characters` |
  | `id` repetido | `duplicate backup id "<id>"` |
  | `name` vazio ou com caractere de controle | `backup name must be one line of text` |
  | `enabled` e `paths` vazio | `a backup needs at least one folder` |
  | pasta relativa depois de expandir `~` | `backup folder must be an absolute path` |
  | pasta repetida | `backup folder "<p>" is listed twice` |
  | pasta dentro de outra | `backup folders overlap: "<a>" and "<b>"` |
  | `dest` nem `conta:caminho` nem caminho absoluto | `backup destination must be an account folder (account:path) or an absolute path` |
  | `dest` de conta sem pasta (`gdrive:`) | `backup destination needs a folder inside the account` |
  | conta fora de `rclone listremotes` | `account "<name>" is not set up in rclone` |
  | `dest` sobrepõe a pasta remota ou local de um sync | `backup destination overlaps "<name do sync>"` |
  | `dest` local dentro de uma pasta do backup | `backup destination is inside a folder being backed up` |
  | `dest` igual a, dentro de ou contendo o de outro backup | `backup destination is used by "<outro name>"` |
  | `dest` local fora de `home`, `/run/media` e `/mnt` | `backup destination is not reachable from the sandbox; use a folder under your home folder, /run/media or /mnt` (só com `FLATPAK_ID`) |
  | `every` desconhecido | `unknown schedule "<v>": use hourly, daily or weekly` |
  | `keep.<k>` fora de 0..=999 | `keep.<k> must be between 0 and 999` |
  | `keep` com chave que não é `daily`, `weekly`, `monthly` nem `yearly` | `unknown keep key "<k>": use daily, weekly, monthly or yearly` |
  | `keep` todo zero | `keep must retain at least one snapshot` |
  | `limit_upload` ou `limit_download` inválido | `limit_upload must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M` (e `limit_download ...`) |
  | preset desconhecido | `unknown preset "<name>"` |
  | linha de filtro sem `- ` | `filter line <n> must start with "- "` |
  | linha `+ ` | `backup filters only exclude: remove line <n> ("+ ")` |
  | linha com `{`, `}` ou `[!` | `backup filter line <n> uses "{...}" or "[!...]", which restic does not support` |

  A sobreposição de remoto segue a regra da fase 1: mesma conta, caminho igual ou prefixo por componente nos dois sentidos, raiz sobrepõe tudo. Remote do tipo `alias` não é resolvido (mesma limitação dos syncs). Pasta de origem inexistente não é erro de config: é estado de rodada (passo 6), porque um disco pode estar desmontado.
- `Paths` ganha, sempre derivados do mesmo resolvedor:
  - `backups_dir()` = `state_dir()/backups`; por backup `backup_state_dir(id)`, `backup_state_file(id)` (`state.json`), `backup_excludes_file(id)` (`excludes.txt`), `backup_runs_dir(id)` (`runs/`), `backup_progress_file(id)` (`progress.json`), `backup_request_file(id)` (`request.json`).
  - `backup_lock_file(id)` = `runtime_subdir()/backup_<id>.lock` (o `_` não existe em id de sync, então não colide com `<id>.lock`).
  - `restic_cache_dir()` = `state_dir()/restic-cache` (`RESTIC_CACHE_DIR`; pode ser apagado sem perda, fica em `state` para os testes controlarem tudo por variável).
  - `restores_dir()`, `restore_file(job)` e `restore_lock_file(job)` para os jobs de restore (passo 9).
  - `dev_keys_dir()` = `data_dir()/dev-keys` (só modo dev, passo 3).
- `$XDG_STATE_HOME/syncfast/backups/<id>/state.json`:

  ```json
  {
    "gate": null,
    "last_outcome": null,
    "offline": false,
    "destination_unavailable": false,
    "unavailable_since": null,
    "consecutive_failures": 0,
    "failing_since": null,
    "last_success": null,
    "last_attempt": null,
    "last_snapshot": null,
    "last_run": null,
    "last_check": null,
    "next_check": null,
    "check_after_failure_done": false,
    "prune_failures": 0,
    "key_unsaved": false,
    "notified": []
  }
  ```

  - `gate`: o mesmo tipo `Gate` do sync (`kind`, `since`, `detail`, `fault`). Os `kind` possíveis são `config_error`, `auth_error` e `needs_attention`; nenhum `GateKind` novo.
  - `last_snapshot`: `{ "id": "<hex>", "time": "<rfc3339>" }`.
  - `last_run`: `{ "started", "finished", "exit_code", "summary": BackupSummary, "log_file" }`, com `BackupSummary { snapshot_id, files_new, files_changed, files_unmodified, data_added, data_added_packed, total_files_processed, total_bytes_processed, warnings }` (`warnings` conta erros de leitura da saída 3).
  - `last_check`: `{ "at", "kind": "quick"|"sample"|"full", "ok": bool, "detail" }`. `next_check`: quando o próximo check de amostra vence.
  - `key_unsaved`: `true` quando um agente criou o backup e o usuário ainda não concluiu `I Saved the Key` (passo 12); backup criado pela GUI ou adicionado com `Add Existing Backup…` nasce com `false`. Não segura nenhuma rodada.
  - Arquivo ausente equivale ao estado padrão: backup não tem revisão de primeira rodada.
  - O estado nunca guarda "running": é o lock do backup (`is_running`), como no sync.
- `Fault` (em `bisync.rs`, usado por `Gate.fault`) ganha as variantes de backup, todas `snake_case` no JSON: `key_missing`, `wrong_key`, `repository_missing`, `source_missing`, `source_empty`, `destination_missing`, `repo_damaged`. As existentes (`auth`, `account`, `quota`, `permission`, `remote_missing`) valem para backup quando o texto do rclone aparece no stderr do restic; `local_empty` não é usada. O `match` de `notify.rs` que hoje cobre `Fault` ganha os braços novos.
- Constantes (`backup/mod.rs`): `CHECK_INTERVAL = 30 dias`, `CHECK_SAMPLE = "5%"`, `AFTER_FAILURE_CHECK = CheckKind::Quick` (estrutural, sem `--read-data`), `PRUNE_FAILURES_BEFORE_GATE = 3`, `STALE_MIN = 48 h`, `STALE_FACTOR = 2`, `RETRY_AFTER = 15 min`. Não há constante de host: `backup_host()` devolve o hostname da máquina (`gethostname`); se a leitura falhar ou vier vazia, a rodada falha como `Transient` com esse erro.
- **Guarda de edição** (`backup/config.rs`): `backup_lowers_safety(before: &BackupConfig, after: &BackupConfig) -> Vec<&'static str>` devolve as chaves que baixam a segurança de um backup: `keep.<k>` com valor menor (inclusive `yearly` de um valor a 0), `paths` que perde algum caminho depois de `~` e normalização (trocar um caminho por outro conta como remover), `dest` diferente, `one_file_system` de `true` a `false`. É a extensão a backup do `lowers_safety` da fase 2; as operações de agente a usam (passo 15) e a GUI não.
- Testes unitários: cada erro da tabela com a mensagem exata, padrões, `~` expandido, sobreposições nos dois sentidos (incluindo raiz), migração `version`, `state.json` sem campo novo carrega com padrão (inclusive `key_unsaved`), `Fault` serializa com os nomes acima, `keep.yearly` (ausente é 0) e `keep` com chave desconhecida, `limit_*` (`off`, `500K`, `2M` e os inválidos), `one_file_system` com padrão `true`, `backup_lowers_safety` (cada chave que baixa e cada alteração que não baixa).

### 3. Chave e KeyStore

Arquivos: `backup/key.rs`, `backup/keystore.rs`.

- **Formato.** 32 bytes do RNG do sistema (`getrandom`), codificados em base32 RFC 4648 sem preenchimento (`data-encoding`, `BASE32_NOPAD`): 52 caracteres `A-Z2-7`. Exibida em 13 grupos de 4 separados por `-`. A senha que o restic recebe é a forma normalizada (52 caracteres, sem `-`).
- **Entrada.** `Key::parse(&str)` tira espaços e `-`, põe em maiúsculas e exige 52 caracteres `A-Z2-7`; senão `The key must have 52 letters and digits (A to Z, 2 to 7).`
- **Tipo.** `Key` guarda a string em `zeroize::Zeroizing`; não implementa `Display` nem `Serialize`; `Debug` imprime `Key(<redacted>)`. A forma exibida vem de `key.display_groups()`, chamada só pela GUI.
- **Trait** `KeyStore`: `get(backup_id) -> Result<Option<Key>, KeyError>`, `put(backup_id, backup_name, &Key)`, `delete(backup_id)`, `backend() -> KeyBackend::{Portal, HostKeyring, Plain, Memory}` e `probe() -> Result<KeyBackend, KeyError>` (abre o armazenamento sem gravar nada, para a GUI e a CLI saberem antes de criar). Implementações:
  - `Oo7KeyStore` (produção). Escolhe o backend nesta ordem, uma vez por processo (`probe`), e guarda a escolha:
    1. **Portal Secret** (`oo7::Keyring::sandboxed()`): arquivo cifrado com o segredo que `org.freedesktop.portal.Secret` entrega ao app. É o preferido.
    2. **Secret Service do host** (`oo7::Keyring::host()`, `org.freedesktop.secrets`, coleção padrão), só quando o portal não existe: o erro do portal é `PortalNotFound`, isto é, o frontend não expõe `org.freedesktop.portal.Secret`. Dentro do Flatpak exige `--talk-name=org.freedesktop.secrets` nos `finish-args`. Fora do Flatpak (modo dev) só existe este.
    3. Nenhum dos dois: `KeyError::Unavailable`, e nenhum backup é criado.

    Qualquer outro erro do portal (backend que falha, pedido negado, keyring bloqueado) **não** cai para o host: vira `KeyError::Unavailable` com o detalhe, para a chave não parar num lugar que o usuário não esperava. O `oo7::Keyring::new()` faz essa queda só a partir da 0.7.0-beta (Fatos); por isso a ordem é escrita aqui, com `sandboxed()` e `host()`, e o backend em uso fica conhecido (`Settings > Backups` mostra qual). Com a 0.6.0, que não tem `sandboxed()` nem `host()`, os dois se montam à mão (`oo7::file` com `ashpd::desktop::secret::retrieve`, e `oo7::dbus::Service`) na mesma ordem (Questões em aberto 4).

    Item com atributos `xdg:schema = "io.github.raphael_soares.syncfast.BackupKey"` e `backup = "<id>"`, rótulo `syncfast backup key: <name>`, `replace = true`. `put` grava só no backend escolhido. `get` procura nele e, se não achar, no outro quando está ao alcance, sem mover o item (assim um backup criado pelo keyring do host continua achando a chave depois que o portal passa a existir, e o contrário). `delete` apaga nos dois.
  - `PlainKeyStore` (só dev e testes): arquivos `<dev_keys_dir>/<id>.key` com modo 0600. Só vale com `SYNCFAST_KEYSTORE=plain`, `SYNCFAST_SYSTEMD=off` e `FLATPAK_ID` ausente; fora disso a variável é ignorada e o log avisa. É insegura e a documentação diz isso.
  - `MemoryKeyStore` (testes unitários e de UI).
- `KeyError::Unavailable(detail)`: `Cannot reach the key storage: <detail>. Backups need the Secret portal or a Secret Service; see docs/guide/backup.md.` Quando o keyring está bloqueado e pede senha, uma rodada sem interface espera no máximo 30 s e falha com `KeyError::Unavailable`; vira a falha `key_missing` do passo 6.
- Regras: a chave nunca é logada (nem em `debug`), nunca aparece em argv ou ambiente de processo, nunca é devolvida por `ops`, nunca vai para `state.json`, run log, `preview.json` ou saída de CLI. `AGENTS.md` ganha a regra "Never log, print or return the backup key".
- Testes: gerar dá 52 caracteres do alfabeto e duas chamadas diferem; `parse` aceita minúsculas, `-` e espaços e recusa tamanho e letras fora do alfabeto; `Debug` não contém a chave; `PlainKeyStore` grava 0600 e ignora a variável com `FLATPAK_ID`; a escolha do `Oo7KeyStore` contra um backend falso por trait interna (portal, host, nenhum): `PortalNotFound` cai para o host, outro erro do portal não, `get` acha a chave no outro backend, `delete` apaga nos dois; roteiro manual do `Oo7KeyStore` nos três cenários da Verificação 4 (CI não tem portal).

### 4. Wrapper do restic e destinos

Arquivos: `restic.rs`, `backup/destination.rs`, `backup/snapshots.rs`.

- **Binário.** `SYNCFAST_RESTIC` (caminho absoluto) se definida; senão `restic` no PATH; senão `ResticNotFound` com `restic not found; reinstall syncfast`. As units e os jobs herdam o caminho como o rclone herda `SYNCFAST_RCLONE`.
- **Processo.** `tokio::process`, args explícitos, sem shell, `current_dir` = pasta de estado do backup, stdin um pipe de onde o restic lê a senha (`--password-file /dev/stdin`; o syncfast escreve a chave normalizada e fecha o pipe), stdout e stderr por pipe. Ambiente: herda o do processo, remove `RESTIC_PASSWORD`, `RESTIC_PASSWORD_FILE`, `RESTIC_PASSWORD_COMMAND`, `RESTIC_REPOSITORY`, `RESTIC_REPOSITORY_FILE` e define `RESTIC_CACHE_DIR=<restic_cache_dir()>` e `RESTIC_PROGRESS_FPS=1`. O `RCLONE_CONFIG` do syncfast segue para o rclone filho.
- **Repositório.** Destino de conta: `-r rclone:<conta>:<caminho>` com `-o rclone.program=<rclone resolvido>` e `-o rclone.args=serve restic --stdio --b2-hard-delete --drive-use-trash=false`. O `--drive-use-trash=false` faz os packs que o `prune` apaga sumirem de vez em vez de ir para a lixeira do Drive, onde continuariam contando na cota; o efeito real no Drive precisa de confirmação (Questões em aberto 1). Destino local: `-r <caminho>`.
- **Argumentos comuns.** Todo comando que abre o repositório real recebe `--limit-upload <n>` e `--limit-download <n>` (KiB/s, opções globais do restic 0.19.1) quando o valor do backup não é `off`: `K` vira `n`, `M` vira `n * 1024` e `G` vira `n * 1024 * 1024`. Valem para `backup`, `forget`/`prune`, `check` e `restore`, porque todos movem dados do repositório; o repositório descartável do `Test Filters` não os recebe. `backup` e `forget` recebem também `--host <backup_host()>`: o hostname real da máquina (`gethostname`), lido uma vez por processo, nunca um nome fixo.
- **SIGINT/SIGTERM.** O processo pai repassa SIGINT ao restic e espera (que sai 130 e solta o lock), como o `run` faz com o rclone.
- **Saída.** `stdout` em JSON lines linha a linha; a última linha `exit_error` do stderr dá `code` e `message`. Texto do rclone dentro do stderr alimenta `fault_of` (só `Auth`, `Account`, `Quota`, `Permission`, `RemoteMissing`).
- **Funções** (cada uma devolve um tipo `Serialize`; todas recebem `&Key`):

  | Função | Comando |
  |---|---|
  | `init` | `init --json` |
  | `verify` | `cat config` (valida repositório e chave; 10 e 12 viram erros tipados) |
  | `snapshots` | `snapshots --json` (todos os hosts) |
  | `backup` | passo 6 |
  | `forget_prune` | passo 6 |
  | `check` | passo 8 |
  | `unlock` | `unlock` (nunca `--remove-all`) |
  | `ls` | passo 9 |
  | `restore` | passo 9 |

  `SnapshotView { id, short_id (8 hex), time, host, paths, files, bytes, added_bytes }`, com `files` e `bytes` de `summary.total_files_processed` e `summary.total_bytes_processed` (ausentes em snapshot sem `summary`).
- **Destino** (`Destination::parse(&str) -> Destination::{Account{name, path}, Local(PathBuf)}`): é de conta quando tem `:` depois de um nome não vazio e não começa com `/`; é local quando é caminho absoluto (depois de `~`); o resto é o erro da tabela do passo 2. `Destination::repo_arg()` monta o `-r`.
- **Criar o repositório** (`create_repository`): antes do `init`, o destino precisa estar vazio ou ausente (`rclone lsjson --no-modtime --no-mimetype <dest>` para conta, `read_dir` para local); senão `The destination folder is not empty.` Depois `init`; se falhar, a chave que a GUI acabou de guardar é apagada (passo 12).
- **Adicionar um repositório existente** (`open_repository`): `verify`; saída 10 vira `No backup was found in that folder.` e 12 vira `That key does not open this backup.`
- Testes: montagem exata de argv por função (snapshot do vetor, sem a chave em lugar nenhum), `Destination::parse` para cada forma, parsers contra os fixtures (`snapshots`, `init`, `exit_error` de 10, 11 e 12), integração com rclone e restic reais em remote `type = local`: init, `verify` com chave certa e errada, `init` em destino não vazio recusado.
- Testes dos argumentos comuns: `K`, `M` e `G` viram os KiB/s esperados, `off` omite a flag, `--host` é o hostname da máquina em `backup` e `forget` e não aparece nos outros comandos.

### 5. Filtros para o restic

Arquivo: `backup/excludes.rs`. Os presets e as linhas livres são os do sync (`filters.rs`), mas o rclone e o restic leem padrões de jeito diferente; a tabela diz como cada regra vira linha do `excludes.txt`.

| Regra do rclone | No restic |
|---|---|
| `- padrão` sem `/` inicial (`- *.log`, `- node_modules/`) | `padrão` sem a barra final (o restic ignora `/` final e então exclui arquivo e pasta com o nome, não só pasta) |
| `- /padrão` (ancorada na raiz da pasta) | uma linha por pasta do backup: `<pasta>/padrão`, com `\ * ? [` escapados na parte `<pasta>` |
| `- a/b` (barra no meio) | igual (os dois casam em qualquer profundidade por componente) |
| `+ padrão` | recusada na validação (passo 2) |
| `{x,y}`, `[!x]` | recusadas na validação |
| preset `git_repos` | uma linha ancorada por repositório encontrado em cada pasta do backup, como `filters::render_live` faz no sync |

- **`$` vira `$$`** em toda linha, porque `--exclude-file` expande variáveis de ambiente (`~$` vira `~$$*` no arquivo).
- Cabeçalho `# Generated by syncfast. Do not edit.`; ordem: filtros livres, presets na ordem de `PRESETS`. Gravado em `backups/<id>/excludes.txt` só quando o conteúdo muda, a cada rodada.
- Mudar filtros **não** abre revisão: excluir mais só afeta snapshots futuros, e os antigos continuam com o que já tinham até envelhecerem. A GUI avisa `Saved. Files you left out stay in older snapshots until they expire.`
- **Presets em backup.** O padrão de um backup novo é `system_junk` e `build_artifacts` (`DEFAULT_BACKUP_PRESETS`). `dev_dirs` não é padrão de backup: exclui `.git/`, ou seja, o histórico dos repositórios, e `git_repos` exclui repositórios inteiros; a GUI mostra, ao ligá-los, `This also leaves out .git folders, so repository history is not backed up.` (`dev_dirs`) e `This leaves out every folder that holds a .git.` (`git_repos`). `build_artifacts` mantém `.git`.
- **`filters.rs`** (compartilhado com os syncs; entregue na P3.1, porque a validação de `presets` do passo 2 precisa do nome). `PRESETS` passa a ter quatro entradas, nesta ordem de render: `system_junk`, `dev_dirs`, `build_artifacts`, `git_repos`. Hoje quatro lugares tratam "todos os presets" como o padrão de um sync: `default_presets()` em `config.rs`, `ops/pairs.rs` ao criar, `ops/rules.rs` no `Test Filters` sem sync e `ui/add_sync.rs` no estado inicial do assistente. Sem mudança, o preset novo ligaria sozinho em todo sync que omite `presets` e na Add Sync, excluiria pastas que o sync hoje leva e mudaria o hash dos filtros. Por isso entram duas constantes e os quatro lugares passam a usá-las: `DEFAULT_SYNC_PRESETS = ["system_junk", "dev_dirs", "git_repos"]` (o que vale hoje) e `DEFAULT_BACKUP_PRESETS`. Em sync o `build_artifacts` é opt-in: aparece na aba Filters (título `Build artifacts and caches` em `preset_title`, `ui/filters.rs`), na CLI e no MCP (`mcp.rs` cita os presets nas descrições), desligado.
- **Preset `build_artifacts`.** Exclui só o que uma ferramenta recria sozinha e nunca `.git`. Cada entrada, com o motivo:

  | Regra | Por que entra |
  |---|---|
  | `- node_modules/` | instalado de novo por `npm`, `pnpm` ou `yarn` a partir do lockfile; costuma ter dezenas de milhares de arquivos |
  | `- target/` | saída do Cargo e do Maven, recriada por `cargo build` ou `mvn package`; o nome é de convenção das duas ferramentas |
  | `- .venv/` e `- venv/` | ambientes virtuais do Python, recriados do `requirements` ou do lockfile; guardam caminhos absolutos, então um restore não os deixaria funcionando mesmo assim |
  | `- __pycache__/` | bytecode que o Python regrava |
  | `- .gradle/` | cache e estado do Gradle por projeto, baixados de novo |
  | `- .cache/` | cache por convenção do XDG; já está em `system_junk`, e a linha repetida não muda o resultado |
  | `- .mypy_cache/`, `- .pytest_cache/`, `- .ruff_cache/`, `- .tox/` | caches e ambientes das ferramentas de tipos e de teste do Python, recriados na próxima execução |
  | `- .parcel-cache/` | cache do Parcel |

  Fora do preset de propósito: `build/`, `dist/`, `out/`, `bin/` e `obj/`. `build/` é a saída de Gradle, CMake e setuptools, mas também um nome comum de pasta com conteúdo real (documentos, projetos de hardware, jogos): excluir por engano perde um arquivo que nenhum comando recria, e incluir só custa espaço, que a deduplicação do restic reduz. `target/` e `venv/` têm o mesmo risco, mas são os mais volumosos e as ferramentas que os criam usam sempre esse nome; ficam por decisão do dono. Quem quer excluir `build/` o acrescenta em `Custom rules`, e a GUI diz isso sob o preset: `Build folders (build/, dist/) are not part of this preset. Add them as custom rules if they only hold output.` O texto do preset na GUI: `Leaves out node_modules, target, .venv and caches. They can be rebuilt.`
- **Test Filters.** Mesma contagem da fase 1 (`<x> of <y> files excluded`, mais `<bytes>`): cria um repositório local descartável em tempdir com senha fixa de teste e roda `backup --dry-run --json` duas vezes (sem e com o `excludes.txt` renderizado) nas pastas do backup; excluídos = diferença de `total_files_processed` e de `total_bytes_processed`. Sem a lista das 20 pastas, porque `excluded_item` não existe no dry run. Cancelável; mostra progresso indeterminado.
- Testes: cada linha da tabela, `$$`, âncora por pasta com caractere especial no caminho, `git_repos` com dois repositórios, `build_artifacts` (cada entrada exclui; `.git/` e `build/` não), `DEFAULT_SYNC_PRESETS` e `DEFAULT_BACKUP_PRESETS` com os valores acima, um sync existente sem `presets` continua com os três de hoje e o hash dos filtros dele não muda, e integração: o `excludes.txt` renderizado exclui o que o `Test Filters` diz (arquivo `~$doc.docx` e pasta `$RECYCLE.BIN` incluídos), nome com `[1]` e espaço.

### 6. Rodada de backup

Arquivos: `backup/run_decision.rs`, `backup/classify.rs`, `backup/run.rs`; CLI interna `syncfast backup-run <id>` (escondida do `--help`, como `transfer-run`).

`decide_backup(input) -> BackupDecision` é função pura. Entrada: `{ enabled, online, dest_is_remote, dest_available, gate, source_check, request }`, em que `source_check` é `Ok`, `Missing(path)` ou `Empty(path)` (pasta ausente, não é diretório, ou diretório sem nenhuma entrada) e `request` é `None` ou `Check(kind)`. A primeira linha que casar vale:

| # | Condição | Decisão |
|---|---|---|
| 1 | `!enabled` | `Skip(Paused)` |
| 2 | gate `auth_error` | `Skip(Gated)` |
| 3 | gate `needs_attention` com fault `repo_damaged` e `request` não é `Check` | `Skip(Gated)` |
| 4 | destino de conta e `!online` | `Skip(Offline)` |
| 5 | destino local e `!dest_available` | `Skip(DestinationUnavailable)` |
| 6 | `request` é `Check(kind)` | `Run(Check(kind))` |
| 7 | `source_check` é `Missing(p)` | `SetGate(needs_attention, source_missing, p)` |
| 8 | `source_check` é `Empty(p)` | `SetGate(needs_attention, source_empty, p)` |
| 9 | senão | `Run(Backup)` |

`dest_available` (destino local) é a existência de `<dest>/config`; um disco desmontado deixa a pasta de montagem vazia e o restic diria "repository does not exist", que não é o caso. Gate `config_error` com o config agora válido é tratada como `null`, como a linha 3b da fase 1.

Fluxo do `backup-run <id>`:

1. Carregar config. Erro global ou `id` inexistente: stderr e saída 2. Backup inválido: gate `config_error` com a mensagem, notifica se for transição, saída 0.
2. `try_lock` exclusivo em `backup_lock_file(id)`. Ocupado: saída 0.
3. Ler `state.json` (inválido: renomeia para `state.json.corrupt-<carimbo>` e segue com estado padrão). Ler `request.json` se existir (`{ "check": "quick"|"sample"|"full" }`), apagando-o ao consumir.
4. Rede (trait `Network` da fase 1) só para destino de conta. `source_check` varre cada pasta (`read_dir`, sem descer).
5. `decide_backup`. `Skip(Paused|Gated)` não grava nada, saída 0. `Skip(Offline)` grava `offline = true`; `Skip(DestinationUnavailable)` grava `destination_unavailable = true` (e `unavailable_since` se estiver nulo); os dois saem **75** (`EX_TEMPFAIL`), para o `Restart=on-failure` da unit (passo 7) tentar de novo em 15 min. Com `unavailable_since` há mais de `max(STALE_FACTOR * período, STALE_MIN)` (definido no item 9), o `Skip(DestinationUnavailable)` também grava a gate `needs_attention` com fault `destination_missing` e detail `<dest> has not been available since <data>`, notificando se for transição; `offline` nunca vira gate (como no sync). `SetGate` grava o gate e notifica se for transição; saída 0.
6. `Run(Backup)`: obter a chave do `KeyStore`. Ausente ou inacessível: gate `needs_attention` com fault `key_missing` (detail da `KeyError`), conta como falha, saída 0. Renderizar o `excludes.txt` (passo 5). Gravar `last_attempt` e limpar `offline`, `destination_unavailable` e `unavailable_since`.
7. `restic backup --json --host <hostname> [--one-file-system] --exclude-file <excludes.txt> <pastas absolutas>`, mais os argumentos comuns do passo 4; `--one-file-system` só com `one_file_system = true`; a chave por stdin. Cada linha `status` reescreve `progress.json` no máximo 1 vez por segundo (`{ "step": "backup", "percent_done", "files_done", "total_files", "bytes_done", "total_bytes", "seconds_remaining", "current" }`); as demais linhas e o stderr vão para `runs/<AAAAMMDD-HHMMSS>.jsonl` precedidos de `{"syncfast_step":"backup"}`. As 50 rodadas mais recentes ficam; `progress.json` é apagado no fim.
8. `classify_backup(exit_code, summary, stderr_lines) -> BackupOutcome`, primeira regra que casar:

   | # | Condição | Outcome |
   |---|---|---|
   | 1 | saída 0 e `summary` com `snapshot_id` | `Ok` |
   | 2 | saída 3 e `summary` com `snapshot_id` | `OkWithWarnings { warnings }` (conta linhas `error` do stderr; o snapshot existe) |
   | 3 | saída 130 | `Cancelled` |
   | 4 | saída 12 | `NeedsAttention { wrong_key }` |
   | 5 | saída 10 | `NeedsAttention { repository_missing }` |
   | 6 | texto casa o padrão de autenticação do rclone (`fault_of` = `Auth`) | `AuthError` |
   | 7 | texto casa `Account`, `Quota`, `Permission` ou `RemoteMissing` | `NeedsAttention { fault, detail }` |
   | 8 | saída 11 | `Locked` (até a fatia P3.3 é tratado como `Transient`; o retry com `unlock` é do passo 8) |
   | 9 | qualquer outro caso, inclusive saída 1, 2 e desconhecida | `Transient { detail }` (a última mensagem `error` ou `exit_error`) |

9. Efeitos:
   - `Ok` e `OkWithWarnings`: `gate = null`, `last_outcome = ok`, `consecutive_failures = 0`, `failing_since = null`, `last_success = agora`, `last_snapshot`, `check_after_failure_done = false`; tira `"failures"` e as chaves `gate:*` de `notified`; grava `last_run`; se `next_check` é nulo, `next_check = agora + CHECK_INTERVAL`. Depois, **retenção** (item 10) e, se `next_check <= agora`, o check de amostra (passo 8). Se `[notifications] success` estiver ligado e o snapshot tiver arquivo novo ou alterado, notifica.
   - `Cancelled`: só `last_attempt`.
   - `AuthError`: gate `auth_error`, notifica se for transição.
   - `NeedsAttention`: gate `needs_attention` com `detail` e `fault`, conta como falha, notifica se for transição (chave `gate:needs_attention:<fault>`).
   - `Transient`: `last_outcome = error`, `consecutive_failures += 1`; o primeiro de uma série grava `failing_since`; ao chegar em `[notifications] failures_after` (padrão 3, 0 desliga) sem `"failures"` em `notified`, notifica e acrescenta. Dispara o check pós-falha (passo 8). Com `failing_since` há mais de `max(STALE_FACTOR * período, STALE_MIN)` (período = 1 h, 24 h ou 7 d para `hourly`, `daily`, `weekly`), grava gate `needs_attention` (sem fault) `Backups have failed since <data>: <última mensagem>`.
10. **Retenção.** Só depois de `Ok` ou `OkWithWarnings`, com `total_files_processed > 0`: `restic forget --host <hostname> --group-by host --keep-daily <d> --keep-weekly <w> --keep-monthly <m> --keep-yearly <y> --prune --json` (flag com valor 0 é omitida, e é assim que `yearly = 0` fica desligado; a chave por stdin). Lê o array da saída só para o log. Falha de `forget` ou `prune` não muda o resultado do backup: grava `prune_failures += 1`, mostra `Old snapshots could not be cleaned up: <mensagem>` no detalhe e, ao chegar em `PRUNE_FAILURES_BEFORE_GATE`, grava gate `needs_attention` (sem fault) com a mesma frase. Sucesso zera o contador. Snapshots de outros hosts (outra máquina no mesmo repositório) são listados na GUI mas nunca apagados: o `--host` restringe a política a esta máquina, e o `--group-by host` a aplica a todos os snapshots dela juntos, inclusive os de listas de pastas anteriores.
11. Saída 0, exceto o 75 do item 5; erro interno do syncfast (I/O de estado) loga e sai 1.

- **Alerta ao reduzir a retenção.** Ao salvar um `keep` em que algum valor diminuiu (GUI, passo 13; inclusive `yearly` indo de um valor a 0), a GUI mostra um alert: `Delete older snapshots?`, texto `The next backup permanently deletes the snapshots outside the new policy from <dest>.`, botões `Cancel` (default) e `Save and Delete Old Snapshots` (destrutivo). Um agente não passa por este alert: reduzir `keep` por ele é recusado (passo 15).
- Testes unitários: `decide_backup` (uma linha da tabela por teste), `classify_backup` (um por regra, com fixtures e texto sintético de cota e autenticação), efeitos de estado por outcome (incluindo o limite `stale`), montagem do argv de `backup` (com e sem `one_file_system`) e de `forget` (com `yearly` 0 e maior que 0, `--host` e `--group-by`), `source_check`.
- Integração (`crates/syncfast-core/tests/backup_local.rs`, ver passo 16): primeiro backup, arquivo novo, exclusão por filtro, retenção, origem ausente, origem vazia, chave errada, chave ausente, saída 3.

### 7. Units e agendamento

Arquivo: `units.rs` (extensão), mais `watch.rs` não muda (backup não usa inotify).

- Nomes `syncfast-backup_<id>.service` e `syncfast-backup_<id>.timer`. O `_` não é permitido em id de sync (`valid_id`), então o namespace é disjunto e `installed_ids` (que ignora nomes cujo id não é válido) não os enxerga; `installed_backup_ids` lê o prefixo `syncfast-backup_`. Mesmo cabeçalho `UNIT_HEADER`, mesmos escapes do passo 8 da fase 1.
- `syncfast-backup_<id>.service`:

  ```ini
  # Generated by syncfast. Do not edit; run "syncfast apply".
  [Unit]
  Description=syncfast backup: <name>

  [Service]
  Type=oneshot
  ExecStart="<bin>" backup-run <id>
  Environment="SYNCFAST_RCLONE=<rclone>"
  Environment="SYNCFAST_RESTIC=<restic>"
  KillSignal=SIGINT
  TimeoutStartSec=infinity
  TimeoutStopSec=180
  Restart=on-failure
  RestartSec=15min
  Nice=15
  IOSchedulingClass=idle
  ```

  O `Restart=on-failure` com `Type=oneshot` é aceito pelo systemd moderno (`systemd.service`: `on-failure` é permitido para oneshot; `always` e `on-success` não). Só o código 75 do `backup-run` o aciona. Confirmar com `systemd-analyze verify` nos testes de render e no roteiro manual. No Flatpak, `ExecStart=flatpak run --die-with-parent --command=syncfast <FLATPAK_ID> backup-run <id>` e sem as linhas `Environment=`.
- **Banda e prioridade.** A banda é por backup (`limit_upload`, `limit_download`) e vai nos argumentos do restic (passo 4), não na unit. `Nice=15` e `IOSchedulingClass=idle` ficam como estão.
- `syncfast-backup_<id>.timer`:

  ```ini
  # Generated by syncfast. Do not edit; run "syncfast apply".
  [Unit]
  Description=syncfast backup timer: <name>

  [Timer]
  OnCalendar=<calendário>
  Persistent=true
  RandomizedDelaySec=10min
  Unit=syncfast-backup_<id>.service

  [Install]
  WantedBy=timers.target
  ```

  | `every` | `OnCalendar=` |
  |---|---|
  | `hourly` | `hourly` |
  | `daily` | `*-*-* 03:00:00` |
  | `weekly` | `Mon *-*-* 03:00:00` |

  `Persistent=true` roda uma execução perdida (máquina desligada às 03:00) assim que o timer volta.
- `apply` (passo 8 da fase 1) passa a reconciliar backups: após as remoções e antes do `daemon-reload`:
  1. Backup removido do config: `disable --now` do timer, `stop` do service, lock bloqueante em `backup_lock_file(id)` por até `PAIR_LOCK_TIMEOUT`, apaga as units e `backups/<id>/`. **Nunca** apaga o repositório nem a chave.
  2. Backup inválido: sem units (as existentes saem como no item anterior, preservando `backups/<id>/`) e gate `config_error` no estado.
  3. Backup válido: grava as units que mudaram; `enabled` → `enable --now` do timer, senão `disable --now`.
- `Trigger` (já existe para rodadas) ganha `start_backup(id)`: `systemctl --user start --no-block syncfast-backup_<id>.service`; em `SystemdMode::Off`, roda `syncfast backup-run <id>` como filho. Um pedido de check grava `request.json` antes do `start`.
- Testes: render exato das duas units para cada `every`, com escapes (`%` no nome, espaço no caminho), variante Flatpak, `installed_backup_ids` ignora unit de sync e vice-versa, `apply` em modo `Off` cria, atualiza e remove (estado apagado, repositório e chave intactos), teste `apply.rs` com backup inválido.

### 8. Integridade

Arquivos: `backup/check.rs`, ajustes em `backup/run.rs`.

- `CheckKind::{Quick, Sample, Full}`:

  | Kind | Comando | Quando |
  |---|---|---|
  | `Quick` | `check --json` | após um backup que falhou (uma vez por série de falhas); `Quick Check` da GUI |
  | `Sample` | `check --read-data-subset=5% --json` | depois de um backup bem-sucedido quando `next_check <= agora` (todo mês); `Check Sample` da GUI |
  | `Full` | `check --read-data --json` | só pelo usuário (`Full Check…`) ou por `syncfast backup check --full` |

  `check` usa um cache novo por padrão; `--with-cache` não é usado, porque um cache velho esconderia dano no destino.
- `parse_check(exit_code, stdout, stderr) -> CheckOutcome::{Ok, Damaged { errors, broken_packs, detail }, Locked, Failed(detail)}`: o JSON `summary` com `num_errors > 0` (ou saída 1 com `broken_packs`) é `Damaged`; saída 11 é `Locked`; o resto, `Failed`.
- Efeitos:
  - `Ok`: `last_check = { at, kind, ok: true }`; `Sample` e `Full` fazem `next_check = agora + CHECK_INTERVAL`; se a gate era `needs_attention` com fault `repo_damaged`, ela só sai com um `Full` sem erro (uma amostra de 5% sem erro não prova que o dano foi consertado).
  - `Damaged`: gate `needs_attention`, fault `repo_damaged`, detail `The check found <n> damaged files in the backup. Backups and cleanup are paused.`; `last_check.ok = false`; notifica se for transição. **Essa gate segura** `backup`, `forget` e `prune` (linha 3 da tabela do passo 6), porque um backup novo sobre um índice que aponta para pack danificado espalharia o dano; checks continuam permitidos. Sai quando um check `Full` termina sem erro. O syncfast não executa `repair`: o detalhe aponta para https://restic.readthedocs.io/en/stable/077_troubleshooting.html, e o reparo guiado é de uma fase futura.
  - `Failed`: conta como falha de rodada (mesmas regras de `Transient`), sem mexer em `last_check`.
- **Check pós-falha.** Depois de um `Transient` (falha sem causa conhecida; conta, cota, permissão, autenticação, chave, origem e destino têm causa própria e não indicam dano) e com `check_after_failure_done = false`, roda `AFTER_FAILURE_CHECK` na mesma rodada e põe `check_after_failure_done = true`; o sucesso de um backup zera a marca. Assim uma série de falhas faz um check só. `AFTER_FAILURE_CHECK` é `Quick`: `restic check` sem `--read-data`, que confere índices, árvores e snapshots sem ler os packs (decidido pelo dono; `--read-data` depois de cada série de falhas baixaria o repositório inteiro do Drive).
- **Locks.** `Locked` (saída 11, de qualquer comando) dispara `unlock` sem `--remove-all` e **uma** nova tentativa do mesmo comando. O `unlock` remove só lock velho do restic (de processo morto na mesma máquina, ou parado há mais de 30 min); um lock vivo de outro processo permanece e a tentativa volta `Locked` de novo, que vira `Transient` com detail `The backup is in use by another process. It will try again.` O syncfast não executa dois comandos de escrita no mesmo backup (lock próprio do passo 6), então um lock que sobra é de rodada morta ou de outra máquina. Com duas máquinas no mesmo repositório, o `prune` de uma segura um lock exclusivo e o backup da outra pode voltar `Locked`; o lock de outro host só é velho depois de 30 min, então o `unlock` não o toca e a rodada tenta de novo na próxima. A detecção "o processo existe" do restic depende do namespace de PID, que no Flatpak pode diferir entre instâncias; se o lock de uma rodada morta parecer vivo, ele envelhece sozinho em 30 min (Questões em aberto 3).
- **Pedido manual.** `request.json` com `check` faz a rodada rodar só o check (linha 6 da tabela do passo 6), sem backup nem retenção, e respeita o lock do backup.
- Testes: `parse_check` contra os fixtures (`check_ok`, `check_sample`, `check_damaged`), efeitos de estado (a gate segura o backup, um `Full` limpo a solta), uma série de três falhas roda um check só, retry de lock com fixture `locked`.
- Integração: backup interrompido com `kill -9` → próxima rodada faz `unlock` e conclui; pack corrompido → `Full` marca `repo_damaged`, rodada seguinte `Skip(Gated)`; restaurar o pack e rodar `Full` solta a gate.

### 9. Snapshots e restore

Arquivos: `backup/snapshots.rs` (listagem, passo 4), `backup/files.rs`, `backup/restore.rs`.

- **Listagem.** `list_snapshots(backup, key) -> Vec<SnapshotView>`, do mais novo para o mais antigo, de todos os hosts.
- **Árvore.** `list_files(backup, snapshot, dir, key) -> Vec<NodeView>` roda `restic ls --json <id> <dir>` (sem `--recursive`) e devolve `{ name, kind (file|dir|symlink|other), path, size, mtime }`. `dir` vazio lista as raízes do snapshot (`/home/raphael/Documents` aparece como a pasta de entrada, por causa dos caminhos absolutos). A GUI carrega uma pasta por vez e guarda em memória. O resultado de `ls` inclui a própria pasta; ela é descartada. Limite de 5000 entradas por pasta; acima disso `truncated: true`.
- **Restore** (`restore(job)`): antes de qualquer coisa, valida:
  1. o snapshot existe no repositório (`latest` vale e é resolvido por `restic snapshots --latest 1`);
  2. o destino **não existe**: a pasta nova é criada pelo syncfast dentro de um pai que existe e é gravável (`Restore destination already exists.` se existir; exit 2). É o que garante que nada é sobrescrito, e vale para GUI, CLI e MCP;
  3. o destino não está dentro da pasta de um sync com rodada em curso (exit 3 `this sync is running; try again when it finishes`), como `copy` e `move`;
  4. cada `--path` pedido é um caminho absoluto do snapshot.
- **Forma do restic.** Caminhos pedidos têm um pai comum `P` (o `path` do snapshot que contém todos):
  - snapshot inteiro com uma só raiz de backup `R`: `restore <id>:<R> --target <destino> --verify`, resultado `<destino>/<conteúdo de R>` (plano);
  - snapshot inteiro com várias raízes: `restore <id> --target <destino> --verify`, com os caminhos absolutos sob `<destino>`;
  - itens escolhidos: `restore <id>:<P> --target <destino> --verify --include /<relativo ao P>` (um `--include` por item, com `\ * ? [` escapados), resultado `<destino>/<relativo>`.
- **Resultado.** `RestoreReport { id, snapshot, destination, files_restored, bytes_restored, total_files, finished, error }`. `total_files == 0` (o restic sai 0 sem fazer nada quando o `--include` não casa) é erro `Nothing in the snapshot matches that path.`, e a pasta de destino criada vazia é removida.
- **Job.** Mesmo molde do `copy` e `move` (ADR 0007): `restores/<id>.json` (job), `restore-<id>.lock` (segurado enquanto roda), processo filho (unit transitória `systemd-run` no host dentro do Flatpak) que roda o `restore-run <id>` escondido; progresso em `restores/<id>.progress.json` a partir das linhas `status` do restic; um job cujo lock sumiu sem resultado vira `failed`. O restore só lê o repositório (lock compartilhado do restic) e roda ao lado de um backup; falha com saída 11 durante um `prune` diz `The backup is busy. Try again in a few minutes.`
- Testes: forma do argv para as três formas, escape de `[1]` e espaço, `total_files == 0`, destino existente recusado sem tocar nada, destino dentro de sync rodando (exit 3), listagem de árvore contra os fixtures `ls_*`; integração: restaurar o snapshot inteiro para pasta nova e comparar com a origem (árvore e conteúdo); restaurar dois itens de pastas diferentes; restaurar depois de apagar a origem; restaurar um snapshot de outro host (criado com `restic backup --host outro`), como numa máquina nova.

### 10. Status e alertas

Arquivo: `backup/status.rs`, `notify.rs` (extensão).

`display_backup_status(backup, state, running) -> BackupStatus`, primeira regra que casar:

| Ordem | Condição | Status | Cor | Ícone | Texto |
|---|---|---|---|---|---|
| 1 | `running`, passo `backup` | BackingUp | `accent` | `refresh-cw` | `Backing up…` |
| 1b | `running`, passo `check` | Checking | `accent` | `shield-check` | `Checking…` |
| 2 | gate `config_error`, `auth_error` ou `needs_attention` | Error | `danger` | `circle-alert` | `Needs attention` |
| 2b | `key_unsaved` | SaveKey | `accent` | `key-round` | `Save the key` |
| 3 | `!enabled` | Paused | `dim` | `pause` | `Paused` |
| 4 | `offline` | Offline | `dim` | `cloud-off` | `Offline` |
| 5 | `destination_unavailable` | Unavailable | `dim` | `hard-drive` | `Destination not available` |
| 6 | `last_outcome = error` e `consecutive_failures >= 3` | Failing | `danger` | `circle-alert` | `Backup failing` |
| 7 | `last_outcome = error` | Retrying | `secondary` | `clock` | `Will retry` |
| 8 | senão | UpToDate | `secondary` | `check` | `Up to date` |

`running` vem do lock (`is_running` sobre `backup_lock_file`); o passo vem de `progress.json` (`step`). A cor nunca é o único sinal.

Notificações (primeiro `Slot::send("notify backup:<id>")`, depois o cliente de notificações, como no passo 7 da fase 1; só a transição de estado notifica; todas respeitam `[notifications]` da fase 2):

| Evento | Título | Corpo |
|---|---|---|
| `needs_attention`, fault `auth` ou `account` | `<name>: backup needs attention` | `Google Drive refused the account. Open syncfast for the fix.` |
| fault `quota` | idem | `Google Drive is full.` |
| fault `permission` | idem | `No permission to the backup folder on Google Drive.` |
| fault `remote_missing` ou `repository_missing` | idem | `The backup folder is missing.` |
| fault `destination_missing` | idem | `<dest> is not available. Plug it in to resume backups.` |
| fault `key_missing` ou `wrong_key` | idem | `The key for this backup is missing on this computer. Open syncfast to enter it.` |
| fault `source_missing` | idem | `<pasta> is missing. Backups resume when it is back.` |
| fault `source_empty` | idem | `<pasta> is empty. If it is on a drive, mount it.` |
| fault `repo_damaged` | `<name>: backup problem` | `The check found damaged data. Backups are paused until you review it.` |
| falha longa (sem fault) | `<name>: backup needs attention` | `Backups keep failing. Open syncfast for details.` |
| `auth_error` | `<name>: account disconnected` | `Reconnect "<remote>" in Settings > Accounts.` |
| falhas em sequência | `<name>: backup failing` | `Failed <n> times in a row. <msg>` |
| sucesso (só com `[notifications] success = true` e arquivo novo ou alterado) | `<name>: backup finished` | `<n> new or changed files, <tamanho> added.` |
| `key_unsaved` (backup criado por agente) | `<name>: save the backup key` | `An agent created this backup. Open syncfast to save the key.` |

Reconectar a conta limpa o gate `auth_error` dos backups daquele remote, como faz para os syncs, e dispara `Back Up Now` neles. Testes: precedência de `display_backup_status` (uma linha por teste), cada texto da tabela e a deduplicação por `gate:needs_attention:<fault>`.

O aviso de `key_unsaved` sai uma vez, quando um agente cria o backup (chave `key_unsaved` em `notified`, retirada quando o usuário conclui `I Saved the Key`), e respeita `[notifications] needs_action`.

### 11. GUI: sidebar, bandeja e Settings

Arquivos: `ui/sidebar.rs`, `tray.rs`, `ui/settings/` (extensão); sem mock aprovado para as telas novas: a primeira atividade da fatia P3.2 (issue #57) é acrescentar os quadros de backup a `docs/design/ui-mock.html` e mostrá-los ao dono.

- **Sidebar.** Abaixo dos syncs, uma segunda seção: cabeçalho `Backups` com dois `icon_button`: `plus` (tooltip `New Backup…`) e `ellipsis` (menu: `Add Existing Backup…`). Uma `sidebar_row` por backup: ícone `archive`, nome, `status_badge` compacto e `subheadline` `Backed up <tempo relativo>` ou `Never backed up`. Sem backups: a seção mostra `empty_state` pequeno `No backups yet` com `New Backup…`. A seleção pode ser um sync ou um backup; o título da janela é o nome do selecionado; `gui.json` guarda `selected_backup` ao lado de `selected_pair`.
- **Bandeja** (itens constantes, desabilita em vez de esconder). A linha de status dos syncs não muda; entram:
  1. logo abaixo dela, uma linha desabilitada `Backups up to date` / `Backing up…` / `<n> backups need attention` / `No backups yet`;
  2. `Back Up All Now` depois de `Sync All Now` (desabilitado sem backups).
  O ponto de atenção e de erro do ícone considera também os backups (`Error` e `Failing` de backup valem como os de sync; `NeedsReview` não existe em backup).
- **Settings > Backups**: painel novo do modal de Settings da fase 2 (#63), num arquivo próprio em `crates/syncfast-gui/src/ui/settings/` (como `accounts.rs`), com uma variante `Backups` em `SettingsPane` (`gui_state.rs`) registrada ao lado de `Accounts`, `Defaults`, `General`, `Notifications` e `Appearance` (ícone na lista de `ui/settings/mod.rs` e o despacho do `show`):
  - `Key storage`: o backend em uso (`Secret portal` ou `Host keyring (Secret Service)`) ou o motivo de não haver (`No Secret portal and no Secret Service answered. See docs/guide/backup.md.`), e `Test Key Storage`, que grava, lê e apaga um item de teste no backend em uso. Com o keyring do host, uma nota: `The Flatpak can read every secret in your keyring. Routing the Secret portal is safer; see docs/guide/backup.md.`
  - `restic`: versão em uso.
  - Link para `docs/guide/backup.md`.
- Testes de UI (`egui_kittest`): seção Backups com e sem itens, seleção alternando entre sync e backup, linhas da bandeja por estado, painel de Settings com portal, com keyring do host e sem nenhum dos dois, status `SaveKey` e sua precedência.

### 12. GUI: criar backup e chave

Arquivo: `ui/sheets/backup_wizard.rs` e backend (`Command::CreateBackup`, `Command::AddExistingBackup`).

**Sheet New Backup** (assistente; Back e Continue no rodapé, Cancel à esquerda; nunca Cancel, Back e Done juntos). Ao abrir, a sheet chama `KeyStore::probe` (passo 3): sem armazenamento de chave (nem portal nem Secret Service), o passo 1 mostra a mensagem da `KeyError::Unavailable` como `inline_error`, com `Continue` desabilitado; com o keyring do host (sem portal), mostra a nota `Using the host keyring. See docs/guide/backup.md.`

1. `Folders`: lista das pastas com `Add Folder…` (`rfd::AsyncFileDialog::pick_folder` pelo portal) e `icon_button` `x` (tooltip `Remove Folder`). Nota: `Folders on another disk inside these folders are not included. Add them as their own folder.` (vale com `Stay on one file system` ligado, o padrão do passo 3). Erros da config (passo 2) aparecem como `inline_error`; Continue desabilitado.
2. `Destination`: `segmented` `Google Drive` e `This Computer`.
   - `Google Drive`: lista de contas (a mesma da Add Sync, com o aviso `Uses rclone's shared client ID`) e `Add Google Drive Account…`; depois `folder_tree` com `New Folder…`, sem a opção `Entire <remote>`. Dica: `Choose a folder only for this backup. Do not use a folder that a sync uses.`
   - `This Computer`: caminho com `Choose…` (portal).
   - Os erros de destino (sobreposição, não vazio, inalcançável) aparecem aqui.
3. `Schedule`: `Back up` com `Every hour`, `Every day` (padrão) ou `Every week`, e o texto `Keep 7 daily, 4 weekly and 6 monthly snapshots.` (com `and <n> yearly` quando `yearly > 0`) com `Customize…`, que abre quatro campos numéricos (`Daily`, `Weekly`, `Monthly`, `Yearly`; `Yearly` em 0 é desligado). Texto de apoio: `Older snapshots are deleted from the destination after each backup.` Abaixo, o disclosure `Advanced` (fechado) com `Stay on one file system` (switch, ligado; desligado mostra `Mounted drives, rclone mounts and Btrfs subvolumes inside these folders are backed up too.`), `Upload limit` e `Download limit` (campos de velocidade da fase 2, `Unlimited` por padrão).
4. `Filters`: a mesma UI da aba Filters do sync (presets com os avisos do passo 5, `Custom rules`, `Test Filters`), com `system_junk` e `build_artifacts` ligados.
5. `Name`: campo com padrão no nome da primeira pasta, `Backup id: <id>` em `footnote` (slug como o do sync, namespace próprio de backup).
6. **`Your Backup Key`** (último passo; Back some; só `Cancel` e o primário):
   - Texto: `This key opens your backup. syncfast cannot recover it, and without it the backup cannot be read. Keep it somewhere safe.`
   - A chave em monospace, em 13 grupos de 4, e `Save Key to File…` (`rfd` salvar; grava `<id>-backup-key.txt` com modo 0600; nota `The file is not encrypted.`).
   - Primário `I Saved the Key`. Enquanto não for apertado, nada foi criado.
   - `Cancel` descarta a chave da memória e não cria nada.
7. Ao apertar `I Saved the Key`, o backend faz, nesta ordem, parando no primeiro erro (mostrado como `inline_error` na própria sheet, com `Try Again`):
   1. `KeyStore::put` (a chave é guardada **antes** de existir repositório, para nunca existir repositório sem chave guardada);
   2. `create_repository` (passo 4); em falha, `KeyStore::delete`;
   3. gravar `[[backup]]` no config (e `version = 3` se for o primeiro); em falha, mantém a chave e o repositório e mostra `The backup was created at <dest>, but syncfast could not save it. Add it with Add Existing Backup…`;
   4. `apply`;
   5. `Back Up Now` (a primeira rodada) e fecha a sheet com o novo backup selecionado.

**Sheet Add Existing Backup** (outra máquina, ou depois de reinstalar): passos `Destination` (igual ao passo 2 do assistente, mas a pasta deve conter um backup), `Key` (campo `Backup key`, mascarado com um `switch` `Show`, aceita o formato com `-`) e `Name`. `Add Backup` roda `open_repository` (passo 4): erros `No backup was found in that folder.` e `That key does not open this backup.` como `inline_error`. Sucesso: guarda a chave, grava `[[backup]]` com `enabled = false` e `paths = []`, roda `apply`, seleciona o backup. A tela do backup oferece `Add Folders…` para configurar pastas e habilitá-lo, ou ir direto a `Browse Files…`.

**Show Key…** (menu `…` do backup): alert `Show the backup key?` com `Anyone who sees this key can read your backed-up files.`, botões `Cancel` (default) e `Show Key`. Depois, a sheet `Your Backup Key` do passo 6 com `Done` e `Save Key to File…`. A chave só existe na memória da GUI enquanto a sheet está aberta.

Quando o backup tem `key_unsaved` (criado por agente), a sheet abre pelo cartão `Save the backup key` (passo 13), sem o alert de confirmação, e o primário é `I Saved the Key` em vez de `Done`; apertá-lo grava `key_unsaved = false` no estado. Só `Cancel` e `I Saved the Key`, e `Cancel` deixa o aviso.

**Enter Key…** (cartão de status com fault `key_missing` ou `wrong_key`): sheet com o campo `Backup key`; `Save Key` verifica com `verify` e guarda; sucesso limpa a gate e dispara `Back Up Now`.

**Alert Remove Backup:** título `Remove "<name>"?`, texto `syncfast will stop backing up these folders. The backup stays at <dest> and can be added again with Add Existing Backup….`, checkbox `Also delete the key from this computer` (desmarcado; marcado, mostra `You will need the key you saved to open this backup again.`), botões `Cancel` (default) e `Remove` (destrutivo). Confirmar remove do config, roda `apply` e, se marcado, `KeyStore::delete`.

Testes de UI: Continue desabilitado com cada erro de validação e a mensagem visível; sem armazenamento de chave, Continue desabilitado com a mensagem de `KeyError::Unavailable`; o último passo só tem `Cancel` e `I Saved the Key`; `Cancel` no passo da chave não emite ação de criação; `I Saved the Key` emite `CreateBackup` com o slug esperado; `Show Key…` pede a confirmação antes de emitir a ação; com `key_unsaved`, `Show Key…` abre com `I Saved the Key` e ele zera o aviso; falha do `put` não cria repositório (teste de backend com `MemoryKeyStore` que falha); `Add Existing Backup…` mostra os dois erros.

### 13. GUI: detalhe do backup

Arquivo: `ui/backup_detail.rs`.

- **Toolbar:** nome (`title2`), `<n> folders → <destino>` em `callout`/`secondary`; à direita `primary_button` `Back Up Now` (desabilitado durante `BackingUp`/`Checking` e com gate que segura, ou sem pastas), `secondary_button` `Pause`/`Resume` e o menu `…` com `Show Key…`, `Check Now` (submenu: `Quick Check`, `Check Sample`, `Full Check…`), `Open Destination Folder` (só destino local, `xdg-open`), separador, `Remove Backup…`. `Full Check…` abre um alert `Read all backup data?` com `This downloads everything from <dest> and can take hours.` e `Cancel` (default) e `Check`.
- **Cartão de status:** `status_badge` grande e uma frase por estado: `Backed up <data e hora>. <n> new or changed files, <tamanho> added.`; com warnings `Backed up <data>. <n> files could not be read.`; `Needs attention`: o `detail` da gate e o botão do conserto (`Reconnect Account…` para `auth_error`, `Enter Key…` para `key_missing` e `wrong_key`, `Check Again` (um `Full Check`) para `repo_damaged`); com `Unavailable`: `<dest> is not available. Backups resume when it is back.`. Durante `BackingUp`: `progress_bar` determinada (`percent_done`) e `<files_done> of <total_files> files · <bytes_done>/<total_bytes> · <seconds_remaining> left`, mais `Current: <arquivo>`; sem total, indeterminada.
- **Cartão `Save the backup key`** (só com `key_unsaved`, acima do cartão de status): `An agent created this backup. Save its key now; without it you cannot restore on another computer.` e o botão `Show Key…`, que abre a sheet do passo 12 com `I Saved the Key`. Não segura o backup nem muda o status de erro.
- **`segmented`:** `Snapshots`, `Activity`, `Settings`.
  - **Snapshots:** `list_row` por snapshot: data e hora, `<files> files · <tamanho>`, `+<added>` e o host quando não é o desta máquina (`backup_host()`). Clicar seleciona; botões na linha: `Browse Files…` (passo 14) e `Restore Snapshot…` (passo 14). Vazio: `empty_state` `No snapshots yet`, com `Back Up Now`. Erro ao listar (rede, chave): `inline_error` com `Try Again`.
  - **Activity:** uma linha por rodada (até 50): data, duração, resultado (`status_badge`), `+<n> new · <tamanho>` e, nas rodadas de check, o tipo (`Quick check`, `Sample check`, `Full check`) e `OK` ou `<n> errors`. Clicar expande as últimas 200 linhas do log em monospace. Vazio: `No activity yet`.
  - **Settings:**
    - `Folders`: lista com `Add Folder…` e `x` por linha, e um aviso se alguma está ausente.
    - `Destination`: texto fixo, sem edição (o destino não muda depois de criado: mudar o destino é criar outro backup).
    - `Schedule`: `Every hour/day/week`.
    - `Keep`: quatro campos numéricos (`Daily`, `Weekly`, `Monthly`, `Yearly`); reduzir um valor aciona o alert do passo 6.
    - `Options`: `Stay on one file system` (switch; desligar abre o alert `Include mounted drives and subvolumes?` com `Folders mounted inside these folders, including Btrfs subvolumes and rclone mounts, are backed up too.` e `Cancel` (default) e `Turn Off`), `Upload limit` e `Download limit` (campo de velocidade da fase 2).
    - Filters: presets, `Custom rules`, `Test Filters`, `Save`.
    - `Save` grava, roda `apply` e mostra `Saved.`; mudar filtros acrescenta `Files you left out stay in older snapshots until they expire.`
- Testes de UI: botões por status, o `detail` e o botão de cada fault, progresso, aba Snapshots vazia e com itens (host de outra máquina visível), redução de `keep` (incluindo `yearly` a 0) abrindo o alert, `Stay on one file system` desligado abrindo o alert, cartão `Save the backup key` só com `key_unsaved`, `Full Check…` abrindo o alert.

### 14. GUI: restore

Arquivo: `ui/sheets/browse_snapshot.rs`, `ui/sheets/restore.rs`.

- **Sheet Browse Files** (título `Files in <data> snapshot`): navegação por breadcrumb e lista da pasta atual (`list_files`, uma pasta por vez, `progress_bar` indeterminada `Reading the snapshot…`); cada linha tem checkbox, ícone (`folder`, `file`), nome, tamanho e data. Selecionar uma pasta seleciona tudo dentro dela (o restore leva a pasta). Rodapé: `<n> items selected`, `Cancel` e `Restore To…` (primário, desabilitado sem seleção).
- **Restore Snapshot…** (na linha do snapshot): salta a seleção e restaura o snapshot inteiro.
- **`Restore To…`:** abre `pick_folder` pelo portal; o syncfast cria dentro da pasta escolhida a pasta nova `<backup name> <AAAA-MM-DD HHMM>` (com ` 2`, ` 3`… se existir) e restaura nela; nunca restaura em cima de algo, nunca no lugar. Antes de começar, a sheet confirma o destino: `Restore to <pasta nova>?`, com a nota `Files are restored into a new folder. Nothing is overwritten.` e botões `Cancel` e `Restore`.
- **Progresso:** `progress_bar` determinada com `<files_restored> of <total_files> files · <bytes>`, `Cancel` (envia SIGINT ao job). Fim: `Restored <n> files to <pasta>.` com `Open Folder` (`xdg-open`) e `Done`; erro: `inline_error` e `Try Again`. O restore roda como job destacado (passo 9): fechar a janela não o interrompe, e a tela do backup mostra `Restoring…` até acabar.
- Se a pasta escolhida está dentro da pasta de um sync com rodada em curso: `This sync is running. Try again when it finishes.` (mesmo texto do review da fase 1).
- Testes de UI: seleção de pasta marca os filhos, `Restore To…` desabilitado sem seleção, a confirmação nomeia a pasta nova, progresso e fim.

### 15. Agentes: ops, CLI e MCP

Arquivos: `ops/backups.rs`, `ops/restore.rs`, `ops/backup_edit.rs`, `syncfast-cli/src/args.rs`, `mcp.rs`. Decisão e limites em ADR 0009 (seção de agentes), que estende ADR 0007.

**Operações** (`Ops`, cada uma devolve structs `Serialize`; CLI e MCP só traduzem, como em ADR 0007):

| Operação | CLI (todas com `--json`) | Ferramenta MCP | Efeito |
|---|---|---|---|
| `Ops::backups` | `syncfast backups` | `list_backups` | todos os backups: `id`, `name`, `status`, `enabled`, `dest`, `paths`, `every`, `keep`, `one_file_system`, `limit_upload`, `limit_download`, `last_success`, `last_snapshot`, `gate` (`kind`, `fault`, `detail`), `key: "stored"|"missing"`, `key_unsaved` |
| `Ops::show_backup` | `syncfast backup show <id>` | `show_backup` | um backup com a última rodada e o último check |
| `Ops::snapshots` | `syncfast backup snapshots <id> [--limit N]` | `list_snapshots` | `SnapshotView` (passo 4) |
| `Ops::snapshot_files` | `syncfast backup files <id> <snapshot> [<pasta>] [--max-entries N]` | `list_snapshot_files` | só metadados: nome, tipo, tamanho, data (padrão 200 entradas, como o `explore` limita pastas) |
| `Ops::backup_now` | `syncfast backup run <id> [--wait N]` | `backup_now` | dispara a unit e espera até N s (padrão 60); o backup segue sozinho depois e `finished: false` diz isso |
| `Ops::check_backup` | `syncfast backup check <id> [--sample\|--full] [--wait N]` | `check_backup` | grava `request.json` e dispara; padrão `quick`; é só leitura |
| `Ops::pause_backup` / `resume_backup` | `syncfast backup pause\|resume <id>\|--all` | `pause_backup`, `resume_backup` | `enabled` (mesmo efeito do botão) |
| `Ops::create_backup` | `syncfast backup add --name <n> --path <p>... --dest <d> [--every <e>] [--keep daily=7,weekly=4,monthly=6,yearly=0] [--preset <p>]... [--filter "- x"]... [--limit-upload <s>] [--limit-download <s>]` | `create_backup` | cria o backup como a GUI (gera e guarda a chave, `create_repository`, config, `apply`, primeira rodada); devolve `id`, `key: "stored"` e `key_unsaved: true`, nunca a chave |
| `Ops::edit_backup` | `syncfast backup set <id> <key> <value>...` (`<key>`: `name`, `paths`, `every`, `keep.daily`, `keep.weekly`, `keep.monthly`, `keep.yearly`, `presets`, `filters`, `limit_upload`, `limit_download`, `one_file_system`, `dest`) | `edit_backup` | grava uma chave depois da guarda abaixo; `paths`, `presets` e `filters` recebem a lista inteira (sem valores, lista vazia) |
| `Ops::remove_backup` | `syncfast backup remove <id>` | `remove_backup` | sempre exit 4 (`NeedsUser`) |
| `Ops::restore` | `syncfast backup restore <id> <snapshot> --to <pasta> [--path <p>]... [--dry-run] [--wait N]` | `restore_snapshot` | job destacado do passo 9 |
| `Ops::restore_job` | `syncfast backup job <id> [--wait N]` | `show_backup_job` | estado do restore (`running`, `done`, `failed`) |

- **Restore para agente:** permitido, só para pasta nova. `--to` é a pasta que será criada (como o destino do `copy`: o destino é o nome novo, não a pasta onde ele entra) e não pode existir (exit 2); mesma recusa de sync rodando (exit 3). `--dry-run` não restaura nem cria pasta: roda `restic restore --dry-run --json` e devolve `total_files` e `total_bytes` que o restore tocaria. O resultado de um restore real mostra pasta, `files_restored` e `bytes_restored`, nunca conteúdo.
- **Sem operação** de mostrar ou aceitar chave. Criar, editar e remover existem pelas linhas acima, sob a guarda abaixo; pausar e retomar valem. Códigos de saída: os de ADR 0007 (0, 1, 2, 3, 4); o 4 ocorre na guarda e em `remove_backup`, e o 5 não ocorre aqui.
- **Guarda** (`backup_lowers_safety(before, after) -> Vec<&'static str>`, em `backup/config.rs`, no molde do `lowers_safety` da fase 2): compara o `[[backup]]` antes e depois e devolve as chaves que baixam a segurança: `keep.<k>` com valor menor (inclusive `yearly` de um valor a 0), `paths` que perde algum caminho (depois de `~` e normalização; trocar um caminho por outro conta como remover), `dest` diferente e `one_file_system` de `true` a `false`. Qualquer chave devolvida dá `OpError::NeedsUser`: exit 4, nada gravado, `This setting lowers safety (<chaves separadas por vírgula>). Change it in syncfast.`; só para `dest` a mensagem é `A backup destination is fixed. Ask the user to create a new backup.`, porque nem a GUI muda o destino. Aumentar `keep`, acrescentar pasta, mudar `every`, `presets`, `filters`, `name` e `limit_*` e ligar `one_file_system` passam. `Ops::remove_backup` não passa pela comparação: sempre devolve `NeedsUser` com `Removing a backup can only be done by the user. Do it in syncfast.`
- **Criar por agente** (`Ops::create_backup`): faz o que a sheet faz, na mesma ordem (passo 12, item 7): valida (passo 2), `KeyStore::put`, `create_repository`, grava o `[[backup]]` (e `version = 3`), `apply`, `Trigger::start_backup`, e grava `key_unsaved = true` no `state.json`. `one_file_system` não é parâmetro: nasce `true`. Sem armazenamento de chave (`KeyError::Unavailable`): exit 1 com a mensagem da `KeyError`, nada criado. Destino não vazio: exit 2 (`The destination folder is not empty.`).
- **Aviso `Save the backup key`:** enquanto `key_unsaved` é `true`, a GUI mostra o status `Save the key` e o cartão `Save the backup key` (passos 10 e 13), e uma notificação saiu uma vez (passo 10). O aviso sai quando o usuário conclui `I Saved the Key` em `Show Key…` (passo 12). Nenhuma operação de agente o limpa, e ele não segura o backup: as rodadas seguem.
- **A chave nunca sai:** `Ops` não tem caminho de retorno para `Key`; `backups` e `show_backup` dizem só `key: "stored"` ou `"missing"` e `key_unsaved`; os processos de `backup run`, `restore` e `backup add` obtêm ou geram a chave dentro do próprio processo e a passam ao `KeyStore` ou ao restic (stdin); nenhum log, JSON ou erro a contém (teste do passo 16).
- **Limite honesto** (como em ADR 0007): um agente roda como o usuário e poderia ler o keyring do mesmo app (com o keyring do host, qualquer processo da sessão lê) ou apagar `excludes.txt`. A guarda impede acidente e deixa o caminho previsto seguro; não é sandbox.
- **`syncfast mcp`:** `instructions` ganha um parágrafo: backups existem; um agente pode criar e editar, mas a chave é do usuário (o syncfast a guarda e o agente avisa o usuário para salvá-la); reduzir retenção, remover pasta ou backup, mudar destino e desligar `one_file_system` é do usuário (exit 4); restore só para pasta nova e avisando o usuário onde ficou.
- **`docs/agents.md`:** nova seção `Backups` (fluxo: `backups`, `backup snapshots`, `backup files`, `backup run`, `backup restore --dry-run` e depois sem, nunca restaurar para dentro de uma pasta de sync sem pedir ao usuário; criar com `backup add` e dizer ao usuário que abra o syncfast e salve a chave), regras de segurança (não pedir, não imprimir, não guardar a chave; restore só para pasta nova; o que a guarda recusa) e a lista de ferramentas MCP atualizada.
- Testes: `ops` com restic e rclone reais (`crates/syncfast-core/tests/ops_backup.rs`): listagem sem chave, `backup_now`, `create_backup` (chave guardada, `key_unsaved`, primeira rodada, destino não vazio recusado, sem armazenamento de chave nada criado), `edit_backup` por chave (cada uma das cinco recusas com a mensagem exata e o config intacto; cada alteração que passa), `remove_backup` sempre exit 4, restore para pasta nova com contagens exatas, restore para pasta existente recusado sem alterar nada, restore para dentro de sync rodando (exit 3), `snapshot_files` só metadados, pausa e retomada; CLI e MCP de ponta a ponta (`crates/syncfast-cli/tests/agent_backup.rs`, `mcp_backup.rs`): `initialize`, `tools/list` com as 13 ferramentas novas, `tools/call` de `list_backups`, `create_backup`, `edit_backup` recusado (`isError`, exit 4) e `restore_snapshot`, erro de pasta existente com `isError`.

### 16. Testes

- **Unitários (core):** os de cada passo acima.
- **Integração** (`crates/syncfast-core/tests/backup_local.rs`): ignorados com aviso se não houver `restic` e `rclone` no PATH (ou `SYNCFAST_RESTIC`/`SYNCFAST_RCLONE`). Cada teste usa `TempDir` com `rclone.conf` (`[testremote]\ntype = local`), `Paths` explícito, `MemoryKeyStore`, `Network` fake e systemd em `Off`; o repositório fica em `testremote:<tempdir>/repo`. Cenários:
  1. criar (init), primeiro backup: snapshot com os arquivos esperados, `*.log` excluído, `~$doc.docx` excluído pelo preset (escape de `$`);
  2. arquivo alterado: segundo snapshot com `files_changed = 1`;
  3. retenção: snapshots antigos criados com `restic backup --time` direto; depois de uma rodada ficam só os da política `7/4/6` (e, com `yearly = 2`, os dois últimos de anos diferentes), e o `prune` removeu os packs sem referência;
  4. origem ausente e origem vazia: gate `source_missing`/`source_empty`, nenhum snapshot novo, repositório intacto; com a pasta de volta, a rodada seguinte limpa a gate;
  5. chave errada e chave ausente: `wrong_key` e `key_missing`, repositório intacto;
  6. destino local indisponível (pasta do repositório renomeada): `Skip(DestinationUnavailable)`, saída 75; passado o limite `stale`, gate `needs_attention`;
  7. arquivo ilegível: saída 3, `OkWithWarnings`, snapshot gravado;
  8. `kill -9` no meio de um backup: a rodada seguinte faz `unlock` e conclui;
  9. pack corrompido: `Full` marca `repo_damaged`, a rodada seguinte é `Skip(Gated)` e a retenção não roda; restaurar o pack e rodar `Full` solta;
  10. restore: snapshot inteiro para pasta nova igual à origem; dois itens de pastas diferentes; destino existente recusado; depois de apagar a origem;
  11. `apply` em modo `Off`: units de dois backups, desabilitar, remover (estado apagado, repositório e chave intactos);
  12. validações de destino: dentro de pasta de backup, sobre pasta de sync, sobre outro backup, raiz de conta;
  13. **a chave não vaza:** com uma chave sentinela, varrer `state/`, `runs/`, `progress.json`, `request.json`, a saída capturada do processo e o `/proc/<pid>/cmdline` do restic durante a rodada, sem achar a chave.
  14. duas máquinas: snapshots criados direto com `restic backup --host outro`; a rodada remove só os do próprio host, e a retenção trata todos os snapshots desta máquina como um grupo (depois de mudar a lista de pastas, os snapshots da lista antiga saem pela política); um segundo `Paths` (outra máquina) abre o mesmo repositório com `open_repository` e a chave;
  15. banda e arquivos: `limit_upload = "512K"` leva um backup de 2 MiB aleatórios a pelo menos 2 s, e o mesmo sem limite leva bem menos; `limit_download` limita o restore; `one_file_system = false` tira `--one-file-system` do argv (o mount real fica no roteiro manual);
  16. guarda de agente: `Ops::create_backup` guarda a chave e roda; `Ops::edit_backup` que reduz `keep`, remove pasta, muda `dest` ou desliga `one_file_system` volta `NeedsUser` sem gravar, e os que aumentam passam; `Ops::remove_backup` sempre volta `NeedsUser`.
- **Fronteira:** `boundary.rs` continua passando com `oo7` (sem egui, eframe, winit nem fastframe de UI). `cargo deny check` passa com as licenças do `oo7`; se uma licença nova aparecer, entra em `deny.toml` com uma linha de motivo.
- **UI:** ver os passos 11 a 14.
- **Mutação:** `just mutants`; meta de zero sobrevivente em `backup/run_decision.rs`, `backup/classify.rs`, `backup/excludes.rs`, `backup/status.rs`, `backup/key.rs` e `backup_lowers_safety` (`backup/config.rs`). Sobrevivente vira teste ou entra em `.cargo/mutants.toml` com uma linha de motivo.

### 17. Docs e ambiente

- `docs/guide/backup.md` (novo, inglês): o que é, criar, a chave (por que existe, onde guardar, `Show Key…`), onde ela fica guardada (portal Secret ou keyring do host), destino, retenção (`keep`, `yearly`, e que a política vale para todos os snapshots desta máquina, mesmo depois de mudar as pastas), limites de banda, `Stay on one file system`, checks, restore passo a passo, restaurar numa máquina nova (`Add Existing Backup…`), duas máquinas no mesmo repositório, backups criados por agentes e o aviso `Save the backup key`, o requisito de armazenamento de chave e como habilitar o portal, erros comuns. `docs/guide/troubleshooting.md` ganha `A backup says Needs attention`, `Key storage is not available` e `Backups are paused after a check`. `docs/guide/privacy.md` ganha o que o restic envia (dados criptografados, só para o destino que o usuário escolheu), onde fica a chave e a regra da chave com agentes. `getting-started.md` ganha uma linha apontando para `backup.md`.
- Texto da entrada `Key storage is not available` (o que o guia diz, em inglês):

  ```text
  Key storage is not available

  syncfast keeps each backup key in your keyring. It asks the Secret portal first.
  If there is no Secret portal, it asks the Secret Service of your session
  (org.freedesktop.secrets). If neither answers, no backup can be created.

  On Hyprland the portal does not use gnome-keyring for secrets by default. Add this
  line under [preferred] in ~/.config/xdg-desktop-portal/hyprland-portals.conf (or in
  portals.conf):

      org.freedesktop.impl.portal.Secret=gnome-keyring

  Then restart xdg-desktop-portal (or log out and in) and check it with:

      busctl --user introspect org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.Secret

  Without the portal, syncfast uses the host keyring and Settings > Backups says
  "Host keyring". That works, but it lets the Flatpak read every secret in your
  keyring. The portal is safer.
  ```

- ADRs: 0009 já existe (esta fatia o cita); ADR 0006 ganha restic e as permissões `/run/media`, `/mnt` e `--talk-name=org.freedesktop.secrets` (esta só para o plano B do Secret Service, com a ressalva do ADR 0009); ADR 0007 já cobre agentes e backup (a guarda e a chave). `AGENTS.md`: Layout cita backup e restic no `syncfast-core` e entra a regra `Never log, print or return the backup key.`
- `PRODUCT.md` é do pai.
- **Nesta máquina** (depois de `just check` verde, junto da instalação da fase 1):
  1. O portal precisa expor `org.freedesktop.portal.Secret`. Hoje não expõe (Fatos). O `~/.config/xdg-desktop-portal/hyprland-portals.conf` já existe e é um link para o dotfiles (`~/.dotfiles/desktop/.config/xdg-desktop-portal/hyprland-portals.conf`, pacote stow `desktop`): acrescentar `org.freedesktop.impl.portal.Secret=gnome-keyring` à seção `[preferred]`, que hoje tem `default=hyprland;gtk` e `org.freedesktop.impl.portal.FileChooser=gtk`. Reiniciar o `xdg-desktop-portal`. Confirmar com `busctl --user introspect org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.Secret`. Se o `UseIn=gnome` do `gnome-keyring.portal` impedir a escolha mesmo com a linha explícita, a alternativa é uma cópia do `.portal` com `UseIn=gnome;Hyprland` em `~/.local/share/xdg-desktop-portal/portals/`. Sem essa mudança o syncfast ainda funciona pelo Secret Service do host (`Host keyring`), com a ressalva do ADR 0009.
  2. `just flatpak`, abrir o app e rodar `Settings… > Backups > Test Key Storage`: precisa dar `Available` e o painel dizer `Secret portal`.

## Processo de construção

Mesmo fluxo da fase 1 (branch por fatia a partir da `main`, Conventional Commits sem `Co-Authored-By`, `just check` verde, revisão por agente independente, PR com `Closes #<issue>`, squash e apagar o branch). As issues #56 a #60 (uma por fatia, título `[P3.N] …`) referenciam os passos desta spec por âncora.

### Fatias

| Fatia | Título | Passos | Depende de | Critério de aceite |
|---|---|---|---|---|
| P3.1 | Backup engine, packaging, key and config | 1, 2, 3, 4, e o `filters.rs` do 5 | fase 2 (`version = 2`) | `[[backup]]` carrega e valida com todas as mensagens (incluindo `keep.yearly`, `limit_upload`, `limit_download` e `one_file_system`); preset `build_artifacts` com `DEFAULT_SYNC_PRESETS` e `DEFAULT_BACKUP_PRESETS` nos quatro lugares do passo 5, sem mudar nenhum sync existente; `restic` 0.19.1 no Flatpak com sha256 e teste de pin, e `finish-args` com `/run/media`, `/mnt` e `org.freedesktop.secrets`; fixtures commitados; `KeyStore` com `MemoryKeyStore`, `PlainKeyStore` e `Oo7KeyStore` (portal, depois Secret Service do host, senão `KeyError::Unavailable`); `backup_lowers_safety` testada; `create_repository` e `open_repository` com restic e rclone reais; a chave não aparece em argv nem em log. |
| P3.2 | Scheduled backups, retention and GUI | 5 (menos o `filters.rs`), 6, 7, 10, 11, 12, 13 | P3.1 | Backup roda por timer systemd e por `Back Up Now`, com exclusões (o `build_artifacts` já vem da P3.1), retenção com `yearly`, limites de banda, `one_file_system` e guardas de origem; hostname real no `--host`; gate `needs_attention` com os faults de backup e as notificações; criar com a chave mostrada uma vez e `I Saved the Key`; cartão `Save the backup key` para backup criado por agente; `Add Existing Backup…`, `Show Key…`, `Enter Key…`, `Remove Backup…`; sidebar, bandeja, Settings > Backups, detalhe com Snapshots, Activity e Settings; mock aprovado; testes de UI. |
| P3.3 | Integrity checks and repair gates | 8 | P3.2 | Check de amostra mensal, check estrutural (`Quick`, sem `--read-data`) pós-falha, `Check Now` na GUI, gate `repo_damaged` que segura backup e retenção, retry de lock com `unlock`; cenários de integração 8 e 9. |
| P3.4 | Browse and restore | 9, 14 | P3.2 | `Browse Files…` e `Restore To…` numa pasta nova com progresso e `Open Folder`; recusas (destino existente, sync rodando); cenário de integração 10; testes de UI. |
| P3.5 | Agent interface for backups | 15 | P3.2, P3.4 | As 13 operações em `ops`, CLI e MCP (criar e editar sob a guarda, remover sempre recusado); a chave nunca sai (cenário 13); cenário 16; `docs/agents.md` e `instructions` do MCP atualizados. |

P3.3 e P3.4 são independentes entre si e só dependem de P3.2; P3.5 espera o restore (P3.4). O passo 16 (testes) e o 17 (docs e portal) são distribuídos: cada fatia entrega os testes do que faz, e o 17 fecha em P3.2 (guia e portal) e P3.5 (`agents.md`).

## Critical files & anchors

- `crates/syncfast-core/src/backup/run_decision.rs` `decide_backup`: a tabela do passo 6, onde uma origem ausente ou vazia impede o snapshot e uma gate `repo_damaged` impede backup e retenção.
- `crates/syncfast-core/src/backup/classify.rs` `classify_backup` e `backup/check.rs` `parse_check`: contrato com o restic, travado pelos fixtures de `tests/fixtures/restic-0.19.1/`.
- `crates/syncfast-core/src/restic.rs`: único lugar que monta argv, ambiente e stdin do restic; a chave só passa por aqui, por stdin.
- `crates/syncfast-core/src/backup/key.rs` e `keystore.rs`: único lugar que vê a chave em texto e que escolhe entre o portal Secret e o Secret Service do host.
- `crates/syncfast-core/src/backup/excludes.rs`: tradução rclone para restic (`$$`, âncoras, recusas).
- `crates/syncfast-core/src/backup/restore.rs`: as recusas de restore (destino novo, sync rodando) que valem para GUI, CLI e MCP.
- `crates/syncfast-core/src/backup/config.rs` `backup_lowers_safety`: a guarda que decide o que um agente pode editar (passo 2); `ops/backup_edit.rs` só a chama.
- `crates/syncfast-core/src/filters.rs`: `PRESETS`, `build_artifacts` e as listas de presets padrão de sync e de backup (passo 5).
- `crates/syncfast-core/src/units.rs`: `installed_backup_ids` e o render das units de backup.
- `packaging/flatpak/io.github.raphael_soares.syncfast.yml`: módulo `restic` e `finish-args`.

## Verification

Diretório `~/Code/Personal/syncfast`.

1. `just check` verde, com os 16 cenários de integração de backup, os testes de UI e `cargo deny check`.
2. `just mutants` sem sobrevivente nos módulos-alvo do passo 16.
3. Roteiro dev (sem Drive, sem systemd, `SYNCFAST_KEYSTORE=plain`):
   1. `just dev-reset && just run-gui-dev`; `+` ao lado de `Backups` abre `New Backup…`.
   2. Pasta `./.dev/docs` com 3 arquivos (um `x.log`) → destino `devremote` pasta `backup` → `Every day` → nome `dev-docs` → a chave aparece em 13 grupos → `I Saved the Key`. O backup aparece `Backing up…` e depois `Up to date`; `Snapshots` mostra 1 snapshot com 2 arquivos (o `.log` fora).
   3. `Back Up Now` de novo: 2 snapshots. Apagar a pasta `./.dev/docs`: `Back Up Now` leva a `Needs attention` com `./.dev/docs is missing.` e nenhum snapshot novo. Recriar a pasta vazia: `is empty`. Repor os arquivos: volta a `Up to date`.
   4. `Browse Files…` num snapshot, marcar um arquivo, `Restore To…` numa pasta de teste: `<pasta>/dev-docs <data>/<arquivo>` existe, igual ao original. Escolher a mesma pasta de novo cria `… 2`, nada é sobrescrito.
   5. `Show Key…` pede confirmação e mostra a mesma chave. Menu `…` > `Full Check…` > `Check`: a atividade registra `Full check OK`.
   6. `Add Existing Backup…` com o mesmo destino e a chave digitada com letras minúsculas e espaços: entra como `enabled = false`; com a chave errada: `That key does not open this backup.`
   7. `syncfast backups --json`, `syncfast backup snapshots dev-docs --json` e `syncfast backup restore dev-docs latest --to /tmp/r1 --json`: sem a chave em nenhuma saída; repetir o `restore` com a mesma `--to` sai 2.
   8. Agente: `syncfast backup add --name dev-agent --path ./.dev/docs --dest devremote:backup-agent --json` cria o backup e já roda; `syncfast backups --json` mostra `key: "stored"` e `key_unsaved: true`, sem a chave em lugar nenhum; na GUI aparece o cartão `Save the backup key` e ele some depois de `Show Key…` > `I Saved the Key`. `syncfast backup set dev-agent keep.daily 1` (reduz) sai com exit 4 e `This setting lowers safety (keep.daily). Change it in syncfast.`, sem mudar o config; `backup set dev-agent keep.yearly 2` passa; `backup set dev-agent one_file_system false`, `backup set dev-agent paths` (sem valores, tira as pastas) e `backup remove dev-agent` saem com exit 4.
   9. Duas máquinas (simulado): `restic backup --host outro ...` direto no repositório, com a chave do `Show Key…`; `Back Up Now` e a retenção não removem o snapshot de `outro`, e a aba Snapshots mostra o host dele.
4. Flatpak e portal (nesta máquina, depois do passo 17):
   1. Com o Secret roteado para o gnome-keyring, `Settings… > Backups > Test Key Storage` dá `Available` e o painel diz `Secret portal`. Criar um backup, fechar e reabrir o app, `Show Key…` mostra a mesma chave; reiniciar a sessão e deixar o timer rodar com o app fechado (a unit lê a chave do portal sem janela).
   2. Tirar a linha `org.freedesktop.impl.portal.Secret` do arquivo do portal e reiniciar o `xdg-desktop-portal`: o painel passa a dizer `Host keyring (Secret Service)`, o backup que já existe continua achando a chave (a leitura procura nos dois) e um backup novo pode ser criado.
   3. Com a linha ainda fora, `flatpak override --user --no-talk-name=org.freedesktop.secrets`: o painel mostra o motivo, o assistente de New Backup abre com `Continue` desabilitado e `syncfast backup add` sai com exit 1 sem criar nada. Desfazer o override e repor a linha ao terminar.
   4. `flatpak run --command=hostname io.github.raphael_soares.syncfast` igual a `hostname` (item 6 dos em aberto).
5. Drive real, depois da migração:
   1. `mkdir -p ~/syncfast-e2e-backup && echo a > ~/syncfast-e2e-backup/a.txt`; criar o backup com a conta `Raphael` / pasta `syncfast-e2e-backup-repo` / `~/syncfast-e2e-backup`. No Drive real só `Raphael:syncfast-e2e-backup-repo` pode ser tocada; nada fora dela.
   2. `rclone lsf Raphael:syncfast-e2e-backup-repo` lista `config`, `data`, `index`, `keys`, `locks`, `snapshots` e nenhum nome de arquivo do usuário.
   3. `systemctl --user list-timers 'syncfast-backup_*'` lista o timer; `systemctl --user cat syncfast-backup_<id>.timer` mostra `OnCalendar=*-*-* 03:00:00` e `Persistent=true`.
   4. Pelo `Show Key…`, pegar a chave e criar 8 snapshots em dias diferentes direto no restic (`restic -r rclone:Raphael:syncfast-e2e-backup-repo backup --host "$(hostname)" --time "<data>" ~/syncfast-e2e-backup`, a chave por `--password-file` num arquivo temporário apagado depois); rodar `Back Up Now`: sobram os snapshots da política. Conferir no Drive que o espaço dos packs removidos foi liberado (`rclone about Raphael:` e a lixeira) para validar o `--drive-use-trash=false` (Questões em aberto 1).
   5. `nmcli networking off && systemctl --user start syncfast-backup_<id>.service`: a unit sai 75 e o status mostra `Offline`, sem notificação; `nmcli networking on` e, em até 15 min, o backup roda sozinho.
   6. Copiar 3 GiB para a pasta: o cartão mostra barra determinada com ETA.
   7. Banda: com `limit_upload` em `1M` na aba Settings, copiar 12 MiB aleatórios novos e rodar `Back Up Now` leva uns 8 s ou mais para enviar (o limitador do restic não tem rajada, ver Fatos, mas 12 MiB deixam folga sobre o tempo de abrir o repositório e se protegem de uma rajada de 4 MiB como a do rclone); com só `limit_download` em `1M` o mesmo envio não é limitado.
   8. Limpeza: `Remove Backup…` com `Also delete the key from this computer`, `rclone purge Raphael:syncfast-e2e-backup-repo`, `rm -r ~/syncfast-e2e-backup`.

## Questões em aberto

Só ficam abertos os itens que se fecham com um teste (fixture, Drive real, Flatpak real); cada um nomeia o teste que o fecha.

1. **`--drive-use-trash=false` no backend rclone.** Escolhido para que os packs apagados pelo `prune` liberem cota. É flag global do rclone (aceita em qualquer remote), mas o efeito no Drive só se confirma com Drive real. Fecha com o teste da Verification 5.4 (`rclone about` e a lixeira antes e depois da retenção). Se a lixeira acumular, plano alternativo: `rclone cleanup` periódico.
2. **Alcance do sandbox para disco externo.** `--filesystem=/run/media` e `--filesystem=/mnt` precisam ser aceitos pelo `flatpak-builder`; senão destino e origem locais ficam restritos à `home`. Fecha com o primeiro `just flatpak` e um backup num disco montado em `/run/media`.
3. **Namespace de PID no Flatpak.** O restic julga um lock "velho" por PID vivo na mesma máquina ou por 30 min sem renovar. Se a detecção de PID falhar dentro do sandbox, um lock de rodada morta dura até 30 min e a rodada devolve `Transient` até lá. Fecha com o teste no Flatpak real: `kill -9` no `restic` de uma rodada e ver se a seguinte faz `unlock` na hora (cenário 8 do passo 16, rodado dentro do Flatpak).
4. **`oo7`, `zbus` e `cargo deny`.** A 0.6.0 não cai para o Secret Service do host quando falta o portal (Fatos); a 0.7.0-beta cai. Falta confirmar que a versão escolhida resolve com o `zbus` 5 em modo `tokio` que o core já usa (um só executor, `zbus ^5.19` na 0.7.0-beta), que `cargo deny check` aceita a árvore (`ashpd`) e que a ordem do passo 3 funciona no Flatpak real: portal, depois host, depois erro. Fecha com `cargo tree -d`, `cargo deny check` e `cargo test -p syncfast-core --test keystore_oo7 -- --ignored` rodado nos três cenários (com portal, sem portal e com o `talk-name`, sem nenhum dos dois). Se a 0.7.0-beta não passar, o passo 3 usa a 0.6.0 com os dois backends montados à mão.
5. **Fixtures.** O formato das linhas `status` do `backup` e do `restore` só se confirma nos fixtures do passo 1; os parsers dos passos 6 e 9 esperam por eles. Fecha com `just capture-restic-fixtures` e os parsers escritos contra os arquivos.
6. **Hostname dentro do Flatpak.** `backup_host()` lê o hostname dentro do sandbox. Se `flatpak run --command=hostname io.github.raphael_soares.syncfast` não devolver o mesmo que `hostname` no host, o modo dev e o Flatpak da mesma máquina teriam hosts diferentes (e snapshots de "outra máquina"). Fecha com esse comando no Flatpak real; se divergir, `backup_host()` lê o hostname do host por `flatpak-spawn --host hostname`.
