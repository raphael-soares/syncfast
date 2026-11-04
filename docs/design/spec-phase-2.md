# syncfast: spec da fase 2

## Contexto

A fase 1 entregou o app completo com padrões sãos e a struct `Behavior` (`crates/syncfast-core/src/behavior.rs`), cujo `for_pair` devolve os mesmos valores para todo par. A fase 2 faz duas coisas:

- **Configuração:** cada campo de `Behavior` passa a ser uma chave do `config.toml`, com um bloco global `[defaults]`, sobrescrita por par, tela na GUI e comando na CLI e no MCP. Entram também os níveis de revisão (`review`), os limites de banda (`limit_upload`, `limit_download`), as notificações configuráveis (`[notifications]`), as opções do app (`[app]`) e o modo de cada sync (`mode`, só por sync).
- **Modos de sync:** além do bisync, um sync pode só enviar, só baixar ou espelhar (apagando no destino), sempre pelo mesmo motor de revisão.

Roadmap:

- **Fase 1 (entregue):** o app com padrões fixos. Specs em `spec-phase-1.md`, ADRs 0001 a 0007.
- **Esta spec (fase 2):** `[defaults]`, herança, `review`, `mode`, banda, notificações, app, spike de Google Docs. ADR 0002 atualizado e ADR 0008 (modos de sync).
- **Fase 3:** backup periódico com restic (spec própria; usa `version = 3` do config e as tabelas `[notifications]` desta fase).

Estado final da fase 2:

- `config.toml` na versão 2, com migração automática da versão 1.
- Um sync herda tudo de `[defaults]` (e dos padrões embutidos) e sobrescreve só o que quiser. A GUI mostra, em cada campo, `Default: 5 min` ou que sobrescreve, com `Reset to Default`.
- Três níveis de revisão (`manual`, `deletions`, `automatic`), com um piso que nenhum nível remove.
- Cinco modos por sync (`mode`, escolhido em cada sync, nunca herdado de `[defaults]`): `bisync`, `upload`, `download`, `mirror_up`, `mirror_down`.
- Limite de banda de envio e de download (`limit_upload`, `limit_download`), herdável, sem limite por padrão.
- Agentes (CLI e MCP) leem e mudam as opções, mas não baixam nenhuma salvaguarda; isso fica para o usuário na GUI.
- Notificações e abertura com a sessão configuráveis. Google Docs continua ignorado, com um spike que decide o assunto a partir de fixtures reais.

## Decisões fechadas

Decididas pelo dono; não reabrir.

### Herança

- Tabela global `[defaults]` e sobrescrita por par de qualquer chave de comportamento, com os mesmos nomes, direto na tabela `[[pair]]`. Chave ausente no par herda; ausente em `[defaults]` usa o padrão embutido (os valores da fase 1).
- Na GUI, cada campo mostra se herda (`Default: 5 min`) ou sobrescreve, com ação de voltar ao padrão.
- `version` do config sobe para 2, com migração da 1.

### Chaves

| Chave | Tipo e valores | Padrão |
|---|---|---|
| `watch` | bool | `true` |
| `debounce` | duração, mínimo `5s` | `"30s"` |
| `max_wait` | duração, mínimo `debounce` | `"5m"` |
| `interval` | duração, mínimo `1m` | `"5m"` |
| `conflict_resolve` | `none`, `newer`, `older`, `larger`, `smaller`, `local`, `remote` | `"newer"` |
| `conflict_loser` | `num`, `pathname`, `delete` | `"num"` |
| `versions` | bool | `true` |
| `versions_keep_days` | inteiro `1..=3650` | `30` |
| `max_delete` | inteiro `1..=100` (percentual) | `50` |
| `resync_mode` | `newer`, `older`, `larger`, `smaller`, `local`, `remote` | `"newer"` |
| `empty_dirs` | bool | `true` |
| `review` | `manual`, `deletions`, `automatic` | `"deletions"` |
| `mode` (só por sync, nunca em `[defaults]`) | `bisync`, `upload`, `download`, `mirror_up`, `mirror_down` | `"bisync"` |
| `limit_upload` | `off` ou velocidade: inteiro positivo seguido de `K`, `M` ou `G` (KiB, MiB, GiB por segundo) | `"off"` |
| `limit_download` | idem | `"off"` |

- `[notifications]`: `needs_action = true`, `conflicts = true`, `failures_after = 3` (0 desliga), `success = false`.
- `[app]`: `autostart = false`, `theme` (`system`, `light`, `dark` ou o nome de um tema do usuário, o mesmo conjunto que a tela Appearance já oferece).
- `extra_args` não existe e nunca será adicionado.
- `google_docs` não é chave: continua `skip` (`--drive-skip-gdocs`) e a chave é rejeitada como desconhecida. O enum `GoogleDocs` fica.
- `mode` só existe no `[[pair]]`. Em `[defaults]` a validação o rejeita com `mode is set per sync, not in [defaults]` (mesma mensagem em `settings set mode <valor>` sem `--sync`, exit 2). Assim um espelho nunca é herdado: quem escolhe um espelho o faz em um sync, de olho nele.

### Revisão

Três níveis, padrão `deletions`:

- `manual`: todo gate pede a confirmação do usuário, como na fase 1.
- `deletions`: o syncfast confirma o gate sozinho quando o dry run não apaga nada em nenhum dos lados, **exceto a primeira rodada de um sync, que sempre espera**.
- `automatic`: o syncfast confirma também as exclusões e a primeira rodada; a GUI mostra um aviso.

A primeira rodada é sempre revisada em `manual` e em `deletions`, mesmo quando o dry run não apaga nada: um par errado (pasta local e remota que não deviam se juntar) mistura duas pastas sem apagar nada, e isso acontece uma vez por sync. Só `automatic` a confirma sozinho.

Piso que nenhum nível remove: fonte vazia (`Fault::LocalEmpty`, e o equivalente do espelho) e erro crítico do bisync param sempre e esperam o usuário. Isso muda o princípio 1 do `PRODUCT.md` e o ADR 0002.

### Agentes (coerente com o ADR 0007)

Baixar uma salvaguarda é ação do usuário. Agentes (comandos da CLI e ferramentas MCP) não podem:

- pôr `review` em `automatic` ou `deletions` (a partir de um nível mais seguro);
- aumentar `max_delete`;
- pôr `versions = false`;
- pôr `conflict_loser = delete`;
- mudar um sync para um modo espelho.

### Modos

- `bisync` (padrão): `rclone bisync`.
- `upload`: `rclone copy` local para remoto, nunca apaga.
- `download`: `rclone copy` remoto para local.
- `mirror_up`: `rclone sync` local para remoto.
- `mirror_down`: `rclone sync` remoto para local.
- Os espelhos usam o mesmo motor de revisão: revisão na primeira rodada, após mudança de filtro e quando o dry run passa de `max_delete`, conforme o nível `review`. Fonte vazia é recusada. O que o destino perde vai para uma pasta de versions via `--backup-dir`.
- Mudar o modo de um sync existente passa por revisão no modo novo, conforme o nível. Os listings do bisync são descartados ao sair do bisync e refeitos por um resync de primeira rodada ao voltar.

### Google Docs

Continua `skip` na fase 2. Uma issue de spike captura fixtures reais de `export` e `url` e decide a partir delas.

### ADRs

ADR 0002 atualizado (níveis, piso, espelho) e ADR 0008 novo (modos de sync).

## Fatos do ambiente (verificados)

- `rclone v1.75.2` (a mesma versão que o Flatpak embute, `packaging/flatpak/io.github.raphael_soares.syncfast.yml`).
- `rclone help flags` (local, v1.75.2): `--backup-dir`, `--max-delete int` ("When synchronizing, limit the number of deletes", padrão -1), `--max-delete-size`, `--filter-from`, `--drive-skip-gdocs`, `--drive-export-formats` (padrão `docx,xlsx,pptx,svg`), `--drive-import-formats` (sem padrão).
- Docs de `rclone bisync` (https://rclone.org/bisync/):
  - `--max-delete` do bisync é **percentual** (padrão 50%). No `sync` é **contagem de arquivos**: "If that limit is exceeded then a fatal error will be generated" (https://rclone.org/docs/#max-delete-int).
  - `--conflict-resolve none` (o padrão do rclone) renomeia os dois arquivos segundo `--conflict-loser` e copia os dois para os dois lados.
  - `--conflict-loser delete` "keeps the winner only"; sem vencedor o rclone ignora `delete` e usa `num`.
  - `--conflict-loser pathname` com `--conflict-suffix path` renomeia o arquivo do Path1 para `file.txt.path1` e o do Path2 para `file.txt.path2`. Sem sufixo explícito vale `conflict` e um número é anexado.
  - `--conflict-*` não valem durante `--resync`; `--resync` só copia.
  - Google Docs são suportados no bisync a partir da v1.66 (docs do rclone). Com `--drive-export-formats url`, apagar o `.url` local apaga o Google Doc correspondente.
- Docs de `rclone sync` (https://rclone.org/commands/rclone_sync/):
  - "Files in the destination won't be deleted if there were any errors at any point."
  - A flag `--combined <arquivo>` (flags de logger) escreve `= path`, `+ path` (só na origem), `- path` (só no destino), `* path` (difere) e `! path` (erro), e funciona como "predictor" com `--dry-run`. Limitações documentadas: tentativas de alto nível duplicam (usar `--retries 1`).
  - `sync` não recusa uma origem vazia: ela apaga o destino. Essa guarda é do syncfast.
- Docs de `--backup-dir` (https://rclone.org/docs/#backup-dir-string): vale para `sync`, `copy` e `move`; o remote precisa suportar move ou cópia no servidor e ser o mesmo do destino; o diretório de backup "must not overlap the destination directory without it being excluded by a filter rule".
- `--bwlimit` (`rclone help flags bwlimit`, v1.75.2: "Bandwidth limit in KiB/s, or use suffix B|K|M|G|T|P or a full timetable"). Docs (https://rclone.org/docs/#bwlimit-bandwidth-spec): bytes por segundo, não bits; sem sufixo é KiB/s; `--bwlimit UP:DOWN` limita envio e download em separado e cada lado pode ser `off`. Testado na v1.75.2 com `--dry-run`: `1M`, `1M:2M`, `1M:off` e `off:1M` valem; `:2M`, `1M:`, `1X` e `1M:2M:3M` falham (`empty string`, `bad suffix 'X'`); o log diz `Starting bandwidth limiter at 1Mi:2Mi Byte/s` (K, M e G são potências de 1024); `rclone bisync --resync --dry-run --bwlimit 1M:off` aceita a flag. Em cópia local para local `UP:DOWN` não limita nada (`1M:off` e `off:1M` terminaram em 0,1 s; `1M` sozinho levou 4,1 s para 4 MiB), então a direção só se mede com um remote de verdade **[ABERTO 5]**.
- Já existem `rclone lsf -R --files-only [--filter-from f] <path>` (`Rclone::list_files`) e `rclone lsjson`.
- Código que a fase 2 mexe, e como ele é hoje:
  - `Behavior::for_pair(&Pair)` é lido por `bisync.rs` `bisync_args`, `units.rs` `pair_timer` (`interval`), `watch.rs` (`watch`, `debounce`, `max_wait`, via `WatchEnv::behavior_for: fn(&Pair) -> Behavior`) e `run.rs` (poda `versions_keep_days`, `preview.rs`).
  - `config.rs` rejeita chave desconhecida com `unknown key "<k>": not supported by this syncfast version` (`reject_unknown_keys`, listas `TOP_LEVEL_KEYS`, `PAIR_KEYS`) e `version` diferente de 1 com `unsupported config version <n>`.
  - A GUI grava o config direto com `Config::edit` (`backend.rs`, `pair_tools.rs`, `sheet_io.rs`); a CLI e o MCP passam por `syncfast_core::ops::Ops` (ADR 0007). `Ops::confirm_review` recusa o que apaga arquivos (exit 4); a GUI confirma gravando o `ConfirmMarker`.
  - Textos que hoje embutem um padrão: `notify.rs` e `ops/views.rs` ("More than half of the files would be deleted"), `ui/review.rs` ("old version kept for 30 days"), `docs/guide/getting-started.md` e `README.md` ("5 minutes", "30 seconds", "30 days").
  - `conflicts.rs` só reconhece `<nome>.conflict<N>`.
  - `notify.rs` usa `NOTIFY_FAILURES_AFTER = 3` (`run.rs`), e `state.rs` `display_status` usa `FAILING_AFTER`.
  - O tema fica em `gui.json` (chave `theme`, `ThemeChoice`: `system`, `light`, `dark` ou o nome de um arquivo de tema do usuário), lido e gravado por `ops/theme.rs`.
- Sessão desta máquina: `graphical-session.target` está `inactive`; o Hyprland não executa `~/.config/autostart/*.desktop`; o syncfast sobe por `hl.exec_cmd("flatpak run io.github.raphael_soares.syncfast --start-hidden")` em `~/.dotfiles/hypr/.config/hypr/autostart.lua`. A GUI já fala com o portal de Settings por zbus.
- Portal de segundo plano (https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Background.html): `RequestBackground(parent_window, options)` com `reason`, `autostart` (b), `commandline` (as), `dbus-activatable` (b); o resultado volta no sinal `Request::Response` com `background` e `autostart`.

## Approach

Os passos estão em ordem de dependência. O passo 1 é a base e vem primeiro. Os passos 2 a 6 só dependem do 1 e podem ser feitos em qualquer ordem ou em paralelo; onde dois deles tocam a mesma tabela, o texto diz qual teste de integração quem entrar depois acrescenta. O passo 7 (spike) não depende de nenhum. Cada passo termina com `just check` verde e os testes nomeados; os de GUI usam `egui_kittest` pela árvore de acessibilidade (AGENTS.md).

Regras que valem em todos os passos:

- Nenhum valor padrão aparece como literal fora de `Behavior::default()`: textos, units e argumentos leem do `Behavior` resolvido.
- Todo texto novo de UI e toda mensagem de validação vão literais como aqui, em inglês, botões em Title Case com `…` quando pedem mais informação.
- O que a GUI grava passa por `Config::edit` e depois `reapply` (o `apply` das units), como hoje.
- Cada passo atualiza os docs que cita (`docs/agents.md`, `docs/guide/`, `README.md`) e o mock `docs/design/ui-mock.html` quando muda tela.

### 1. Config v2, `[defaults]` e herança

Arquivos: `config.rs`, `behavior.rs`, novo `settings.rs`, `ops/settings.rs`, CLI, MCP, GUI.

Base de todas as outras. Entrega as onze chaves que `Behavior` já tem (`watch`, `debounce`, `max_wait`, `interval`, `conflict_resolve`, `conflict_loser`, `versions`, `versions_keep_days`, `max_delete`, `resync_mode`, `empty_dirs`) editáveis de ponta a ponta. `limit_upload` e `limit_download` entram no passo 2; `review`, `mode`, `[notifications]` e `[app]` entram nos passos 4, 6 e 5, cada um registrando suas chaves no mesmo registro; até lá a chave é rejeitada como desconhecida.

**Schema v2:**

```toml
version = 2

[defaults]
debounce = "10s"
conflict_resolve = "local"

[[pair]]
id = "notes"
name = "Notes"
local = "~/Notes"
remote = "gdrive:Notes"
enabled = true
presets = ["system_junk", "dev_dirs"]
filters = []
interval = "15m"
```

- `[defaults]` e as chaves de comportamento no `[[pair]]` são opcionais. Valor do par vence `[defaults]`, que vence o embutido. Exceção: `mode` só vale no par e é rejeitado em `[defaults]` (passo 6).
- Duração é string: um ou mais pares `<inteiro><s|m|h>` sem espaço (`30s`, `5m`, `1h30m`). Outra forma dá `invalid duration "<texto>": use a number followed by s, m or h, for example 30s, 5m or 1h30m`. A escrita canônica usa a maior unidade que divide exato (`300s` vira `5m`). Parser próprio em `behavior.rs`, com testes (não depende do formato amigável do jiff).
- Posição: `[defaults]` é criada logo depois de `version` (`Table::set_position`), nunca depois dos `[[pair]]`.

**Migração v1 para v2:**

- `Config::parse` aceita `version = 1` ou `2`. Um v1 é lido pelo schema v2 (todas as chaves novas são opcionais; um v1 que traga `[defaults]` ou chave de comportamento no par falha com `unknown key "<k>": not supported by this syncfast version`, como hoje) e `Config::migrated_from() -> Option<i64>` devolve `Some(1)`.
- `Config::save` sempre grava `version = 2` (edita só o inteiro, preserva comentários e ordem). Logo, qualquer edição migra.
- Migração explícita: `Config::migrate(paths)` (sob o `config.lock`) é chamada por `syncfast apply` e na abertura da GUI. Se o arquivo é v1, copia o original para `config.toml.v1` ao lado do arquivo real (segue o symlink do stow; não sobrescreve uma cópia que já exista) e regrava só a linha `version`. Não escreve `[defaults]`.
- `version` diferente de 1 e 2: `unsupported config version <n>` (inclui o 3 da fase 3 em um app antigo).

**Registro de chaves (`settings.rs`):** uma tabela estática `KEYS` com, por chave: nome, escopo (`Behavior`, `Notifications`, `App`), `per_sync_only` (só `mode`: rejeitada em `[defaults]` com `mode is set per sync, not in [defaults]`, fora da visão `defaults` de `settings` e fora do painel `Defaults` da GUI; a origem de `mode` é só `builtin` ou `sync`), tipo (bool, duração, velocidade, inteiro com limites, enum), padrão (de `Behavior::default()`), `validate`, `applies_to(SyncMode)`, e `lowers_safety(before, after) -> bool`. Parsing, validação, `reject_unknown_keys` (as listas `PAIR_KEYS` e das tabelas saem do registro), CLI, MCP e GUI leem dele; chave nova é uma linha e um teste.

**Resolução:**

- `Config::defaults() -> Behavior`: o embutido com `[defaults]` aplicado.
- `Behavior::for_pair(defaults: &Behavior, pair: &Pair) -> Behavior`: `defaults` com as sobrescritas do par. `Pair` ganha `overrides: Overrides` (campos `Option` por chave, preenchidos pelo registro; `Pair` deixa de usar `deny_unknown_fields` do serde, porque `flatten` e `deny_unknown_fields` não convivem, e a rejeição fica no `reject_unknown_keys` que já percorre o documento).
- `Config::effective(&Pair) -> Effective { behavior: Behavior, origins: Map<chave, Origin> }`, com `Origin ∈ {Builtin, Defaults, Sync}`.
- Consumidores migrados (clean cutover, nenhum chama `Behavior::for_pair(&Pair)` antigo):
  - `run.rs` `lookup_pair` devolve também os `defaults` do config carregado; `Round` e `preview.rs` usam `Behavior::for_pair(&defaults, &pair)`.
  - `units.rs` `write_units` usa `config.defaults()`.
  - `watch.rs`: `WatchEnv::behavior_for` passa a `fn(&Behavior, &Pair) -> Behavior` e `load_pairs` lê os `defaults` do config que já carrega (o watcher já recarrega ao mudar o config).
- Textos que dependiam de padrão passam a ler o `Behavior` do par: `notify.rs` (duas ocorrências) e `ops/views.rs` dizem `More than {max_delete}% of the files would be deleted. Review before continuing.` (e `...: review before continuing.` na view); `ui/review.rs` diz `(old version kept for {versions_keep_days} days)` e, com `versions = false`, `(old version not kept)`; `docs/guide/getting-started.md` e `README.md` deixam de afirmar números fixos ("by default every 5 minutes").

**Erros.** Globais (`GlobalConfigError`, impedem carregar qualquer par), prefixo `config.toml: `:

- `[defaults]` inválida: `config.toml: [defaults]: <mensagem de chave>`.
- Chave desconhecida dentro de tabela: `unknown key "defaults.<k>": not supported by this syncfast version` (idem `notifications.<k>`, `app.<k>`). Chave desconhecida no par continua `unknown key "<k>": ...`.

Por par (`PairError`, o par fica fora do `apply` e os demais seguem). Mensagens de chave, usadas nos dois lugares:

- `debounce must be at least 5s`
- `max_wait must be at least debounce`
- `interval must be at least 1m`
- `versions_keep_days must be between 1 and 3650`
- `max_delete must be between 1 and 100`
- `unknown value "<v>" for <key>: use <lista de valores separada por vírgulas e "or">`
- `<key> must be <bool|a duration|a whole number>` para tipo errado
- `conflict_loser "delete" needs a conflict_resolve that picks a winner` quando o efetivo é `conflict_resolve = none` com `conflict_loser = delete` (o rclone ignora `delete` sem vencedor e usa `num`; o syncfast não aceita a combinação para ninguém acreditar que ela apaga)

A validação de relação entre chaves (`max_wait >= debounce`, `conflict_loser`) roda sobre o `Behavior` efetivo do par: um `[defaults]` que combine com a sobrescrita de um par e quebre a regra invalida **o par**, não o arquivo.

**Edição (`Config`):** `set_setting(scope, key, value)` e `reset_setting(scope, key)`, com `scope ∈ Defaults | Pair(id)`; `reset` remove a chave da tabela (toml_edit, preservando o resto). Sem guarda: é o caminho do usuário (GUI). Gravar no par um valor igual ao herdado ainda cria a sobrescrita; `Reset to Default` é que a remove.

**Ops, CLI e MCP (`ops/settings.rs`):**

- `Ops::settings(sync: Option<&str>) -> SettingsView`, `Ops::set_setting(sync, key, value) -> SettingChanged`, `Ops::reset_setting(sync, key)`. `SettingsView`:

  ```json
  {
    "scope": "defaults",
    "settings": [
      { "key": "debounce", "value": "30s", "origin": "builtin", "default": "30s", "applies": true }
    ],
    "lowered_safety": []
  }
  ```

  Com `sync`, `scope` é o id, `origin ∈ builtin|defaults|sync`, `default` é o valor que a chave tomaria após `reset` e `applies` diz se a chave vale no modo do sync. Sem `sync`, `scope = "defaults"` e `default` é o embutido. As chaves de `[notifications]` e `[app]` entram na visão de `defaults` com prefixo (`notifications.success`, `app.autostart`) quando os passos 5 as registram.
- CLI: `syncfast settings [--sync <id>]`, `syncfast settings set <key> <value> [--sync <id>]`, `syncfast settings reset <key> --sync <id>` (e `settings reset <key>` sem `--sync` remove a chave de `[defaults]`). Todos aceitam `--json`. `<value>`: `true`/`false`, inteiro ou texto. Exit 2 para chave, valor ou combinação inválidos (a mensagem é a do config), 4 para a guarda abaixo, 1 para o resto. Um `settings set` que muda units chama `reapply`, como `set_filters`.
- MCP: `get_settings { sync? }`, `set_setting { sync?, key, value }`, `reset_setting { sync?, key }` (`value` é um valor JSON). Mesmas respostas e mesmos erros (`isError` com a mensagem da CLI).
- `pair_view`/`show` ganham `lowered_safety` (abaixo) e, quando existir, `mode`.

**Guarda dos agentes (mecanismo fechado nesta spec):**

- O caminho do usuário e o do agente são os mesmos que o ADR 0007 já separa: a GUI grava com `Config::set_setting`/`reset_setting` dentro de `Config::edit`, sem guarda; CLI e MCP só têm `Ops::set_setting`/`reset_setting`, que aplicam a guarda. Não existe flag nem variável para contorná-la, como na confirmação de revisão.
- A guarda calcula o `Behavior` efetivo de **todo** sync antes e depois da mudança (num `Config` em memória, sem gravar). Se alguma entrada `KEYS[k].lowers_safety(before, depois)` for verdadeira para algum sync, nada é gravado e a operação falha com `OpError::NeedsUser`: exit 4, `This setting lowers safety (<chaves separadas por vírgula>). Change it in syncfast.`. Isso cobre `settings set` em `[defaults]`, em um par e `settings reset` (voltar a um padrão menos seguro também é baixar).
- `lowers_safety` por chave (um para cada passo que registra a chave): `max_delete` sobe; `versions` vai de true a false; `conflict_loser` vai de qualquer valor a `delete`; `review` vai para um nível menos seguro (`manual` > `deletions` > `automatic`, passo 4); `mode` vai para `mirror_up` ou `mirror_down` e é diferente do anterior (passo 6). Mudar para o mesmo valor, ou para um mais seguro, passa.
- Limite aceito, igual ao do ADR 0007: é guarda comportamental, não sandbox. Um agente roda como o usuário e poderia editar `config.toml`. Para o desvio não ficar invisível, `lowered_safety(effective)` lista as chaves do `Behavior` efetivo que estão menos seguras que o padrão embutido (mesmos comparadores); aparece em `show`, `status --json` e como o selo `Safety lowered` no detalhe do sync na GUI, com a lista na dica. Quem escreveu o arquivo não importa.
- `docs/agents.md`: o exit 4 passa a dizer "only the user can do this: confirm a review that deletes files, or lower a safety setting"; seção Safety rules ganha a lista acima; lista de ferramentas MCP ganha as três.

**GUI (casca, com as onze chaves):**

- Nova aba `Options` no detalhe do sync (depois de `Filters`; `DetailTab::Options`) e novo painel `Defaults` em Settings. Painéis de Settings depois da fase 2: `General` (passo 5), `Accounts`, `Defaults`, `Notifications` (passo 5), `Appearance`.
- As duas telas desenham as mesmas seções a partir do registro: `Schedule` (`watch`, `debounce`, `max_wait`, `interval`), `Conflicts` (`conflict_resolve`, `conflict_loser`), `Versions` (`versions`, `versions_keep_days`), `Deletions` (`max_delete`), `Advanced` (`resync_mode`, `empty_dirs`); o passo 2 acrescenta `Bandwidth` e o passo 4 acrescenta `Review` às duas, e o passo 6 acrescenta `Mode` só à aba `Options` (o painel `Defaults` não o tem, porque `mode` é por sync). Campos que não valem no modo do sync (`applies = false`) não são desenhados na aba `Options`.
- Rótulos e controles:

  | Chave | Rótulo | Controle | Rótulo dos valores |
  |---|---|---|---|
  | `watch` | Watch for changes | switch | On, Off |
  | `debounce` | Wait after changes | campo de duração | `30 s`, `5 min`, `1 h 30 min` |
  | `max_wait` | Longest wait while editing | campo de duração | idem |
  | `interval` | Check Drive every | campo de duração | idem |
  | `conflict_resolve` | When both sides change | combo | Keep both, Newer wins, Older wins, Larger wins, Smaller wins, This computer wins, Drive wins |
  | `conflict_loser` | The losing version | combo | Rename as .conflictN, Rename as .path1 or .path2, Delete |
  | `versions` | Keep replaced files | switch | On, Off |
  | `versions_keep_days` | Keep for | campo inteiro com sufixo `days` | `30 days` |
  | `max_delete` | Stop when more than this share would be deleted | campo inteiro com sufixo `%` | `50%` |
  | `resync_mode` | When a first sync finds different files | combo | Newer wins, Older wins, Larger wins, Smaller wins, This computer wins, Drive wins |
  | `empty_dirs` | Sync empty folders | switch | On, Off |

- Cada linha, na aba `Options`: sem sobrescrita, legenda `Default: <valor herdado>` em cor `dim`; com sobrescrita, legenda `Overrides the default (<valor herdado>)` e o botão de ícone `Reset to Default` (ícone `rotate-ccw`, nome acessível igual) que emite `Action::ResetSetting`. No painel `Defaults` a legenda é `Built-in default: <valor>` quando o campo foi alterado e o botão `Reset to Default` remove a chave de `[defaults]`. Um filho que sobrescreve mostra, no painel `Defaults`, `Overridden by <n> syncs` (sem lista).
- Campos de duração e inteiro gravam com Enter ou ao perder o foco; a mensagem de validação do core aparece como erro em linha (`inline_error`) logo abaixo e nada é gravado. Enum e bool gravam ao mudar.
- `Action::SetSetting { scope, key, value }` e `Action::ResetSetting { scope, key }`; o backend chama `Config::edit(set_setting)` e depois `reapply`.
- Atualizar `docs/design/ui-mock.html` com a aba `Options` e o painel `Defaults`.

**Testes:**

- Config: `a_v1_config_loads_and_reports_it_was_migrated_from_1`, `the_first_save_of_a_v1_config_writes_version_2_and_keeps_comments`, `migrate_copies_a_v1_file_to_config_toml_v1_once_and_leaves_a_v2_file_alone`, `a_v1_config_with_a_defaults_table_is_an_unknown_key_error`, `version_3_is_unsupported`, um teste por mensagem de validação acima (global e por par), `a_defaults_value_that_breaks_a_pairs_relation_invalidates_only_that_pair`, `set_setting_creates_defaults_right_after_version`, `reset_setting_removes_the_key_and_keeps_other_comments`.
- `Behavior`: `for_pair_uses_the_sync_value_then_defaults_then_builtin`, `origins_name_where_each_value_came_from`, parser de duração (válidos, `1h30m`, `5 min`, vazio, `0s`, `90`), escrita canônica.
- Consumidores: `bisync_args` com `Behavior` resolvido (um teste por chave), `pair_timer` com `interval` do par, `watch` ignora o par com `watch = false` e usa o `debounce` do par, poda usa `versions_keep_days` do par; textos de `max_delete` e de versions.
- Ops: `settings_lists_every_key_with_origin_and_default`, `set_setting_on_a_sync_writes_an_override_and_reapplies_units`, `reset_setting_removes_the_override`, guarda: `an_agent_cannot_raise_max_delete`, `an_agent_cannot_turn_versions_off`, `an_agent_cannot_set_conflict_loser_delete`, `an_agent_can_lower_max_delete`, `reset_to_a_less_safe_default_is_refused_for_agents`, `a_refused_change_writes_nothing`, `lowered_safety_lists_keys_less_safe_than_the_builtin_default`; CLI e MCP: mesma resposta, exit 4.
- GUI (`tests/ui_options.rs`, `ui_settings.rs`): linha herdada mostra `Default: 5 min`; sobrescrita mostra `Overrides the default (5 min)` e `Reset to Default`, que emite a ação; duração inválida mostra `debounce must be at least 5s` e não emite `SetSetting`; campo que não vale no modo não aparece; painel `Defaults` mostra `Overridden by 2 syncs`; selo `Safety lowered`.

`just check` verde.

### 2. Agendamento, watcher e banda

Chaves: `watch`, `debounce`, `max_wait`, `interval`, `limit_upload`, `limit_download`. Arquivos: `units.rs`, `watch.rs`, `behavior.rs`, `bisync.rs`, GUI.

Depende só do passo 1.

- Semântica por modo (valem com o passo 6; antes dele, só o `bisync` existe e vale tudo):
  - `interval` vale em todos os modos: é `OnUnitInactiveSec` do timer do sync (`units.rs` `pair_timer`), a próxima rodada `interval` depois do fim da anterior. Mínimo `1m`.
  - `watch`, `debounce` e `max_wait` valem nos modos cuja origem é a pasta local (`bisync`, `upload`, `mirror_up`). Em `download` e `mirror_down` o watcher ignora o sync (olhar a pasta local não tem o que propagar; reagir a edição local desfaria a edição) e a aba `Options` esconde as três.
  - `max_wait >= debounce` (passo 1); o watcher já dispara no mais cedo de `último evento + debounce` e `primeiro evento + max_wait`.
- Efeito ao vivo:
  - `settings set interval` e a GUI chamam `reapply`. `apply` reescreve o timer do sync quando o texto muda e, só para o timer reescrito de um sync habilitado, executa `systemctl --user restart <timer>` depois do `daemon-reload` (hoje só há `enable --now`, que não reinicia um timer em execução). **[ABERTO 1]** confirmar com uma unit real que o `daemon-reload` sozinho não aplica o intervalo novo a um timer em execução; se aplicar, o restart sai.
  - O watcher recarrega o config ao mudar (`watch_config`) e recalcula `debounce`, `max_wait` e `watch` do sync sem reiniciar nem soltar os outros syncs.
- **Banda (`limit_upload`, `limit_download`):** limita a velocidade das rodadas do sync, herdável de `[defaults]`, padrão sem limite.
  - Valor: `off` (sem limite, o padrão) ou um inteiro positivo seguido de `K`, `M` ou `G`, sem espaço e sem decimais: `500K`, `2M`, `1G`. `K`, `M` e `G` são KiB, MiB e GiB por segundo (potências de 1024, como no rclone; é bytes, não bits). Minúscula é aceita; a escrita canônica usa maiúscula e a maior unidade que divide exato (`1024K` vira `1M`). Parser próprio em `behavior.rs` (`Bandwidth { Off, KiBPerSec(u64) }`), com testes.
  - Validação, uma só mensagem por chave: `limit_upload must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M` (e `limit_download ...`). Caem nela `0`, `0K`, decimais, número sem unidade, negativo, e as unidades `B`, `T` e `P`. O `0` fica de fora porque no rclone `0` é "sem limite"; no syncfast é `off`.
  - Herança normal: `off` no par sobrescreve um limite de `[defaults]` para sem limite; `Reset to Default` volta a herdar.
  - Argumento: `bisync_args` (e `oneway_args`, quando o passo 6 entra) acrescentam `--bwlimit <UP>:<DOWN>`, com UP do computador para o Drive e DOWN do Drive para o computador. Os dois lados são sempre escritos, com `off` no lado sem limite (`off:1M`, `2M:off`; o rclone recusa `:1M` e `2M:`), e a flag é omitida quando os dois são `off`. Vale também no dry run (que não transfere nada, então não muda nada). Não vai para `lsf`, `purge` nem outros comandos auxiliares.
  - Por modo (`applies_to`, passo 6): `limit_upload` vale em `bisync`, `upload` e `mirror_up`; `limit_download` vale em `bisync`, `download` e `mirror_down`.
  - Vale na próxima rodada: o rclone lê os argumentos a cada execução, então não há `reapply` nem reinício; uma rodada em curso termina com o limite antigo.
  - Não baixa nenhuma salvaguarda: sem `lowers_safety`, agentes (CLI e MCP) mudam o limite sem restrição e `lowered_safety` não o lista.
  - A direção do limite só se confirma com um Drive real (**[ABERTO 5]**, seção Itens em aberto).
- GUI (seções `Schedule` e `Bandwidth`, passo 1): ao lado dos campos de `Schedule`, uma linha de resumo do efetivo, `Syncs every 5 min and 30 s after you stop editing.` (com `watch = false`: `Syncs every 5 min. Changes on this computer wait for the next round.`). Em `[defaults]` a linha não aparece. A seção `Bandwidth` vem depois de `Schedule`, nas duas telas:

  | Chave | Rótulo | Controle | Rótulo dos valores |
  |---|---|---|---|
  | `limit_upload` | Limit uploads to | campo de velocidade | `Unlimited`, `500 KiB/s`, `2 MiB/s`, `1 GiB/s` |
  | `limit_download` | Limit downloads to | campo de velocidade | idem |

  O campo aceita o mesmo texto do TOML (`2M`); vazio ou `off` grava `off` e mostra `Unlimited`. Grava com Enter ou ao perder o foco; valor inválido mostra a mensagem do core em linha e não grava. Cada linha mostra `Default: Unlimited` (ou o limite herdado) e `Reset to Default`, como as outras. Os dois campos aparecem só quando valem no modo do sync.
- Docs: `docs/guide/getting-started.md` passa a dizer "by default" e aponta para a aba `Options`; `docs/guide/settings.md` (passo 5) e `docs/agents.md` citam as chaves de banda.

**Testes:**

- Agenda: `pair_timer` com `interval` de `90s`, `1h`; `apply` reescreve só o timer do sync mudado (units fake) e reinicia só esse; `watch_ignores_the_sync_when_the_mode_is_download_or_mirror_down` (entra com o passo 6); `a_config_change_updates_debounce_of_one_sync_and_keeps_the_others_watched`; resumo da GUI para cada combinação; `SYNCFAST_SYSTEMD=off`: gravar `interval` regrava a unit de preview.
- Banda: parser (`500K`, `2M`, `1G`, `2m`, `off`; recusa `0`, `0K`, `1.5M`, `10`, `-1K`, `5T`, vazio); escrita canônica (`1024K` vira `1M`); `bisync_args` com `2M:off`, `off:512K`, `1M:2M` e sem a flag quando os dois são `off`; `a_sync_off_overrides_a_defaults_limit`; `an_agent_can_change_bandwidth_limits_either_way`; GUI: `Default: Unlimited`, `2 MiB/s`, vazio grava `off`, `0` mostra a mensagem e não emite `SetSetting`, campo oculto quando a direção não vale no modo. Com o passo 6, o mesmo teste de `oneway_args` (quem entrar depois o acrescenta).

`just check` verde.

### 3. Conflitos e versões

Chaves: `conflict_resolve`, `conflict_loser`, `versions`, `versions_keep_days`. Arquivos: `bisync.rs`, `conflicts.rs`, `notify.rs`, GUI.

Depende só do passo 1.

**Fixtures primeiro** (como o passo 2 da fase 1): `crates/syncfast-core/tests/capture_fixtures.sh` ganha cenários, gravados em `fixtures/rclone-1.75.2/`:

| Cenário | O que faz |
|---|---|
| `conflict_none` | arquivo alterado nos dois lados, `--conflict-resolve none` |
| `conflict_pathname` | idem com `--conflict-resolve newer --conflict-loser pathname --conflict-suffix path` |
| `conflict_pathname_none` | idem com `--conflict-resolve none --conflict-loser pathname --conflict-suffix path` |
| `conflict_delete` | `--conflict-resolve newer --conflict-loser delete` |
| `conflict_local_wins` | `--conflict-resolve path1` |

O que cada um mostra (nomes dos arquivos resultantes nos dois lados, a linha de aviso do conflito, o que o `--backup-dir1` recebe) é lido dos fixtures, nunca suposto. **[ABERTO 3]** o nome exato (`file.txt.path1`) vem da doc; os fixtures confirmam.

- **Argumentos (`bisync_args`):** `--conflict-resolve` com `ConflictResolve::rclone_value()` (já mapeia `local` para `path1` e `remote` para `path2`); `--conflict-loser` com `ConflictLoser::rclone_value()`; com `Pathname`, acrescenta `--conflict-suffix path`; com `Num` e `Delete`, nenhum sufixo (vale o `conflict` do rclone).
- **Conflitos pendentes e aba Conflicts:** `conflicts.rs` ganha `Naming { Num, Pathname }`. `.conflict<N>` é sempre reconhecido (sobra de configuração anterior); `.path1` e `.path2` só quando o `conflict_loser` efetivo do sync é `pathname`. Com `Pathname`, o "número" que `keep_both_name` usa é o dígito do sufixo. `find_conflicts`, `list_conflicts` e `count_conflicts` recebem o `Naming`. Com `conflict_loser = delete` não sobra arquivo: a aba mostra, no estado vazio, `Conflicts are resolved by deleting the losing version. Change this in Options.` Com `conflict_resolve = none` os dois lados ficam renomeados e o original não existe (`current: None`, caso que a aba já trata).
- **Notificação de conflito:** com `conflict_loser = delete`, `Alert::NewConflicts` diz `<name>: <n> conflicts resolved` / `The losing version was deleted. Local copies stay in the versions folder.` (sem "Both versions were kept").
- **Versions (`versions`, `versions_keep_days`):** `--backup-dir1` como hoje; `versions = false` não passa `--backup-dir1`. A poda por idade continua rodando a cada rodada com `versions_keep_days` do sync, também com `versions = false` (pastas antigas somem por idade). Nos passos do modo (6) a mesma chave governa o `--backup-dir` dos modos `download`, `mirror_up` e `mirror_down`; `upload` não perde nada local e a chave não vale.
- **GUI (seções `Conflicts` e `Versions`, passo 1):**
  - `conflict_loser = Delete`: aviso em linha, cor `warning`, ícone `triangle-alert`: `The losing version is deleted. On this computer it stays in the versions folder for 30 days; on Drive it goes to the trash.` (os 30 vêm de `versions_keep_days`; com `versions = false` a frase é `The losing version is deleted and not kept.`).
  - `versions = Off`: `Files replaced or deleted on this computer are not kept.`
  - Nenhum `alert` modal: o aviso é suficiente (ADR 0004).
  - Combinação inválida (`none` com `delete`): o erro de validação em linha, nada é gravado.
- Docs: `docs/guide/getting-started.md` ("kept for 30 days" passa a "by default").

**Testes:** `bisync_args` por chave e `pathname` com sufixo; `classify` e `summarize` com cada fixture novo (o que o `summarize` conta em `conflicts_new` para `delete`); scanner reconhece `.path1` só com `pathname` e `.conflict3` sempre; `keep_both_name` com `pathname`; integração local↔local com os quatro valores de `conflict_loser` (o arquivo certo sobrevive, o perdedor tem o destino certo, o versions recebe o local substituído); `versions_false_passes_no_backup_dir`; `prune_runs_with_versions_off`; combinação `none`+`delete` recusada no config e no `Ops`; GUI: aviso de `Delete`, aviso de versions off, estado vazio da aba Conflicts.

`just check` verde.

### 4. Níveis de revisão

Chave: `review`. Arquivos: `run_decision.rs`, `run.rs`, `preview.rs`, `state.rs`, GUI.

Depende só do passo 1. A chave `review` (`manual`, `deletions`, `automatic`; padrão `deletions`) entra no registro com `lowers_safety`: o nível novo é menos seguro que o anterior na ordem `manual` > `deletions` > `automatic`.

**Que gates o syncfast confirma sozinho.** Só os que a fase 1 resolve com um dry run e uma confirmação:

| Gate | `manual` | `deletions` | `automatic` |
|---|---|---|---|
| `needs_review_first_run` | espera | espera sempre, mesmo sem exclusões | confirma |
| `needs_review_filters_changed` | espera | idem | confirma |
| `needs_review_mode_changed` (passo 6) | espera | idem | confirma |
| `all_files_changed` | espera | idem | confirma |
| `too_many_deletes` | espera | confirma só se o dry run (com `--force`) não apagar nada; na prática espera | confirma |
| `needs_review_critical_error` | espera | espera | espera (piso) |
| `needs_attention` (inclui `Fault::LocalEmpty`, fonte vazia do espelho), `auth_error`, `config_error`, `local_missing` | não são revisão | idem | idem (piso) |

"Apaga nada" é o mesmo critério do ADR 0007 para o agente: `ChangeCounts.delete_local == 0 && delete_remote == 0` (função única `ChangeCounts::deletes()`, usada também por `Ops::confirm_review`). Sobrescritas não contam: o local substituído vai para versions e o remoto, para as revisões do Drive (ADR 0002). A conta vale para todos os gates da tabela menos `needs_review_first_run`, que em `deletions` espera sempre (um par errado junta duas pastas sem apagar nada, e a primeira rodada só acontece uma vez por sync).

**Piso.** Nenhum nível altera:

- `NeedsReviewCriticalError` nunca é confirmado sozinho.
- Uma fonte vazia nunca vira revisão: o bisync a recusa pelo `Empty current Path1 listing` (`Fault::LocalEmpty`, `needs_attention` na hora) e o espelho a recusa antes de rodar o rclone (passo 6).

**Onde roda.** Dentro do `run` do sync, que já segura o lock do par, logo depois de `decide` e antes do aviso de gate:

1. `decide` devolve `SetGate(k)` ou `Skip(Gated)` com um gate de revisão `k` da tabela acima, e o nível efetivo não é `manual`.
2. Se `gate.auto_checked == Some(nível efetivo)`, não repete (evita um dry run a cada rodada de um gate que espera). Senão roda o dry run (`preview` refatorado para uma `preview_locked` que usa o lock já segurado, com o modo do sync) e aplica a tabela por uma função pura `auto_review(level, kind, &ChangeCounts) -> AutoReview::{Confirm, Hold}`.
3. `Confirm`: grava o `ConfirmMarker { kind: ReviewKind do gate, filters_hash }` (o mesmo marcador da GUI) e reavalia `decide` uma vez; a rodada segue como `Resync` ou `Force`, sem aviso de gate.
4. `Hold`: grava `gate.auto_checked = Some(nível efetivo)` e segue o caminho de hoje (anuncia o gate; a GUI e o agente fazem a revisão manual como sempre).
5. Falha do dry run (rede, rclone): o gate fica como estava, sem `auto_checked`, e o erro entra como uma falha transitória da rodada.

- `Gate` ganha `auto_checked: Option<ReviewLevel>` (`serde(default, skip_serializing_if = "Option::is_none")`). Mudar o nível de revisão (ou o `[defaults]`) faz a próxima rodada tentar de novo, porque o nível efetivo deixa de ser igual ao `auto_checked`.
- `after_filters_saved` (que já põe o gate na hora) não muda: a rodada seguinte aplica a tabela.
- **Registro:** `LastRun` ganha `auto_reviewed: Option<AutoReviewed { gate, level, counts }>`. A aba Activity mostra `Reviewed automatically` e as contagens na linha da rodada; `activity --json` inclui o campo. Com `[notifications].success` ligado, uma revisão automática que apagou arquivos notifica `<name>: reviewed automatically` / `Deleted <n> files after an automatic review.`
- **Efeito do padrão novo:** nenhum no primeiro sync: em `manual` e em `deletions` o gate `needs_review_first_run` espera e a sheet de Review abre depois de `Create Sync`, como na fase 1. Só com `review = automatic` o sync recém-criado vai direto a `Syncing…`: a GUI não abre a sheet, o passo 3 de boas-vindas (`Review`) fica concluído com o texto `Review confirmed automatically, so syncing started.` e, quando o syncfast já confirmou, `add` responde `next_step: "Syncing started: review confirmed automatically."`. O agente continua pelo `review show` e `review confirm`, que não mudam.
- **GUI (seção `Review` das telas do passo 1):** combo `Review` com `Ask every time`, `Ask only for deletions` (padrão) e `Never ask`, e a legenda de cada um:
  - `Ask every time`: `Every first sync, filter change, mode change and mass deletion waits for you.`
  - `Ask only for deletions`: `A review is confirmed automatically when its preview deletes nothing. The first sync of a folder always waits for you.`
  - `Never ask`, em cor `warning` com ícone `triangle-alert`: `Deletions are confirmed automatically, including mass deletions. Only an empty source folder or a critical error stops a sync.`
- ADR 0002 já descreve os níveis e o piso (atualizado nesta fase). Atualizar `PRODUCT.md` princípio 1 (feito pelo pai): "...a review gates anything risky... At the default level syncfast confirms it itself when the preview deletes nothing, except the first sync of a folder, which always waits for you; deleting files, a critical error and an empty source always wait for you, unless you choose Never ask for deletions."

**Testes:**

- `auto_review`: tabela inteira (nível x gate x apaga ou não), `a_critical_error_is_held_at_every_level`.
- Decisão: `an_auto_confirmed_first_run_runs_as_resync_without_a_gate_notice`.
- Integração local↔local: a primeira rodada fica em `needs_review_first_run` em `manual` e em `deletions` mesmo sem nenhuma exclusão (`a_first_run_waits_for_review_at_deletions_even_when_nothing_is_deleted`) e confirma sozinha em `automatic`; os outros gates confirmam sozinhos em `deletions` quando o dry run não apaga nada e ficam em espera com exclusões (o remoto ter um arquivo que será sobrescrito não conta; um arquivo só no destino em modo espelho conta, após o passo 6); `automatic` confirma `too_many_deletes` e o remoto perde os arquivos; `manual` mantém o comportamento da fase 1 (todos os testes da fase 1 rodam com `review = "manual"` explícito); `LocalEmpty` em `automatic` vira `needs_attention` e o remoto mantém todos os arquivos (`an_emptied_local_folder_needs_attention_at_every_review_level`); `auto_checked` evita o segundo dry run; mudar o nível refaz.
- Ops: `an_agent_cannot_set_review_to_automatic`, `an_agent_can_raise_review_to_manual`, `confirm_review_and_the_automatic_check_use_the_same_deletes_rule`.
- GUI: o combo e as três legendas, o aviso de `Never ask`, `Reviewed automatically` na Activity, boas-vindas concluída sem sheet só em `automatic` e a sheet aberta depois de `Create Sync` em `deletions`.
- Se o passo 6 já entrou: `a_mode_change_waits_for_review_at_manual_and_confirms_at_deletions_when_nothing_is_deleted`; se não, o passo 6 acrescenta esse teste ao entrar.

`just check` verde.

### 5. Notificações e app

Tabelas: `[notifications]`, `[app]`. Arquivos: `notify.rs`, `run.rs`, `state.rs`, `ops/theme.rs`, GUI.

Depende só do passo 1. Registra no registro as chaves `notifications.needs_action`, `notifications.conflicts`, `notifications.failures_after`, `notifications.success`, `app.autostart`, `app.theme`; sem `lowers_safety`. `failures_after` é `0..=100`; fora disso, `failures_after must be between 0 and 100`. A fase 3 usa as mesmas chaves para backups.

**Notificações (`notify.rs`, lidas pelo `run` a cada rodada, sem estado novo):** `Policy { needs_action, conflicts, failures_after, success }`, carregada do config que `lookup_pair` já abre. O filtro roda em `send`, **depois** de `announce`/`announce_gate` registrarem a chave em `State::notified`: desligar uma categoria não gera uma rajada de avisos antigos quando se religa.

| Alert | Categoria | Regra |
|---|---|---|
| `Gate { .. }` (todos os tipos, inclui revisão, deleções, auth, local ausente, atenção, erro de config) | `needs_action` | enviado só se ligado |
| `NewConflicts` | `conflicts` | enviado só se ligado |
| `Failing` | `failures_after` | enviado quando `consecutive_failures >= failures_after` e `failures_after > 0`; `0` desliga; o texto usa o valor (`Failed <n> times in a row.`) |
| `Synced { uploaded, downloaded, deleted }` (novo) | `success` | uma rodada `Ok` com ao menos uma mudança; título `<name>: synced`, corpo com as partes diferentes de zero (`3 uploaded, 1 downloaded, 2 deleted`); rodada sem mudança nunca notifica |
| `AutoReviewed` (passo 4) | `success` | uma revisão automática que apagou arquivos (`delete_local + delete_remote > 0`) |

- A escalada para `needs_attention` após 6 horas de falha (`ESCALATE_AFTER`) é um gate e segue `needs_action`.
- `state.rs` `display_status`: o limite de `Failing` passa a vir de um parâmetro `failing_after` (o `failures_after` efetivo se `> 0`, senão 3, o padrão), e `FAILING_AFTER` e `NOTIFY_FAILURES_AFTER` deixam de ser constantes. A etiqueta `Sync failing` não é notificação e continua aparecendo com `failures_after = 0`.
- A GUI e o tray não mudam de estado: só as notificações do desktop.

**App:**

- **Tema:** `[app].theme` passa a ser o lugar do tema. Aceita `system`, `light`, `dark` e o nome de um arquivo de `themes/` do usuário (o mesmo conjunto de `ThemeChoice`, que a tela Appearance já oferece). A chave `theme` do `gui.json` é lida uma vez, na abertura da GUI: se o config não tem `app.theme` e o `gui.json` tem um valor diferente de `system`, o valor é copiado com `Config::edit` e a chave sai do `gui.json` (geometria e demais chaves ficam). `ops/theme.rs` (`syncfast theme`, MCP `theme`) lê e grava `[app].theme` e continua enviando `reload-themes` à janela. Config com `theme` vazio: `theme must not be empty`. Tema nomeado que não existe segue o caminho de erro atual da Appearance (volta a `system` e mostra o erro).
- **Autostart (só na GUI, só no Flatpak, gate `FLATPAK_ID`):** a GUI chama `org.freedesktop.portal.Background.RequestBackground` por zbus com `parent_window = ""` e `options { reason: "Start syncfast hidden when you log in", autostart: <config>, commandline: ["syncfast-gui", "--start-hidden"], dbus-activatable: false }`.
  - Quando chama: ao mudar `app.autostart` (a GUI relê o config a cada poll) e na abertura, só se o valor aplicado por último (`gui.json` `autostart_applied`) for diferente do config. Evita pedir de novo a cada abertura.
  - Resposta: `Response` com `autostart` igual ao pedido grava `autostart_applied`. Resposta negada ou diferente: o config **fica** como o usuário pediu, a linha mostra o erro `The desktop did not allow autostart.` e `autostart_applied` não muda.
  - `syncfast settings set app.autostart true` pela CLI só grava o config; o efeito vem na próxima vez que a GUI roda (a nota fica na ajuda do comando).
  - Fora do Flatpak (`just run-gui-dev`) a linha fica desabilitada com a legenda `Available in the Flatpak install.`
  - Se `XDG_CURRENT_DESKTOP` contém `Hyprland`, a legenda diz `Hyprland does not run autostart entries. Add an exec line for syncfast to its config.` (o Hyprland desta máquina já sobe o app por `autostart.lua`). **[ABERTO 4]** o comportamento do portal em GNOME e KDE (entrada gravada em `~/.config/autostart`) não pode ser verificado nesta máquina; o teste manual da seção Verificação cobre.
- **GUI:**
  - Painel `General`: `Start syncfast when you log in` (switch) e a legenda acima.
  - Painel `Notifications`: `Needs action` (switch, `Notify when a sync needs your review or attention.`), `Conflicts` (switch, `Notify when a round keeps two versions of a file.`), `Failures` (campo inteiro, `Notify after this many failed rounds in a row. 0 turns it off.`, mostra `Off` com 0), `Success` (switch, `Notify when a round changed files.`).
  - Painel `Appearance`: o seletor de tema grava `[app].theme`.
  - Cada linha mostra `Default: <valor>` e `Reset to Default` como as outras (mesmo widget do passo 1, escopo `defaults`).
- CLI e MCP: `settings` lista e grava `notifications.<chave>` e `app.<chave>`; `syncfast theme` continua.
- Docs: `docs/guide/` ganha `settings.md` (opções, herança, notificações); `docs/agents.md` cita `settings`.

**Testes:** `Policy` por categoria (`a_gate_notice_is_dropped_when_needs_action_is_off_but_still_recorded_as_notified`, `new_conflicts_respect_conflicts`, `failing_uses_failures_after_and_zero_turns_it_off`, `a_successful_round_notifies_only_when_it_changed_files`); `display_status` com `failing_after` 5 e 0; `failures_after must be between 0 and 100`; migração do tema de `gui.json` para o config (uma vez, preserva o resto); `syncfast theme` grava `[app].theme`; portal com um servidor zbus falso: pede só quando o valor difere do aplicado, grava `autostart_applied` na resposta positiva, mostra o erro na negativa e não repete; GUI: painéis, `Off` com 0, legenda do Hyprland, linha desabilitada fora do Flatpak.

`just check` verde.

### 6. Modos de sincronização

Chave: `mode`. Arquivos: novo `oneway.rs`, `behavior.rs`, `bisync.rs`, `run_decision.rs`, `run.rs`, `state.rs`, `config.rs`, `notify.rs`, GUI.

Depende só do passo 1. Este passo também atende o ADR 0008. `review` (passo 4) decide quem confirma; sem ele, todo gate é manual.

**Renomeio:** `bisync::Mode` (`Normal`, `Resync`, `Force`, `DryRun(ReviewKind)`) passa a se chamar `RoundMode`, porque agora serve aos dois motores; é mecânico e os testes mantêm os nomes. `SyncMode { Bisync, Upload, Download, MirrorUp, MirrorDown }` é o novo enum de `behavior.rs` (`rclone_value`/`as_str` com os nomes do TOML). `mode` entra em `Behavior` e no registro como chave só por sync (`per_sync_only`, passo 1: `[defaults]` a rejeita com `mode is set per sync, not in [defaults]`), com `lowers_safety` verdadeiro quando o modo novo é `mirror_up` ou `mirror_down` e diferente do anterior (também espelho para espelho: inverter a direção apaga do outro lado).

**Fixtures primeiro** (`capture_fixtures.sh`, cenários `oneway_*`, com `RCLONE_CONFIG` vazio e remotes locais, gravados em `fixtures/rclone-1.75.2/`; o log JSON e, para cada um, o arquivo do `--combined`):

| Cenário | O que faz |
|---|---|
| `copy_new` | `copy` com arquivos novos e um que já existe igual |
| `copy_changed` | `copy` com arquivo alterado na origem e mais novo no destino |
| `sync_delete` | `sync` com arquivo só no destino |
| `sync_delete_dry_run` | o mesmo com `--dry-run` |
| `sync_backup_dir` | `sync` com `--backup-dir` para alterado e apagado |
| `sync_max_delete` | `sync --max-delete 1` com 3 arquivos a apagar (código de saída e texto) |
| `sync_empty_source` | `sync` com origem vazia, sem a guarda (para registrar que ele esvazia o destino) |
| `sync_errors` | `sync` com um arquivo ilegível na origem (o rclone não apaga nada no destino) |
| `size_json` | `rclone lsf -R --files-only` numa origem vazia e numa com arquivos |

Coisas que os fixtures confirmam, listadas como **[ABERTO 2]**: o `--combined` com `--dry-run` traz `+`, `*` e `-` iguais aos da rodada real; `--retries 1` evita linhas duplicadas; o código de saída e a mensagem de `--max-delete` excedido; e que `lsf -R --files-only` termina quando o primeiro nome é lido e o processo é encerrado, sem travar numa origem remota.

**Argumentos (`oneway_args(pair, behavior, paths, mode: RoundMode, now)`, ordem fixa):**

| Modo | Subcomando | Origem, destino | `--backup-dir` |
|---|---|---|---|
| `upload` | `copy` | local, remoto | nenhum |
| `download` | `copy` | remoto, local | `<data>/versions/<id>/<stamp>/` |
| `mirror_up` | `sync` | local, remoto | `<account>:.syncfast-versions/<id>/<stamp>` |
| `mirror_down` | `sync` | remoto, local | `<data>/versions/<id>/<stamp>/` |

```
<copy|sync> <origem> <destino>
--filter-from <state>/filters/<id>.txt
[--max-delete <N>]                       só mirror_*, só RoundMode::Normal
[--create-empty-src-dirs]                empty_dirs
--drive-skip-gdocs
[--backup-dir <dir>]                     versions e a tabela acima
--combined <state>/pairs/<id>/runs/<AAAAMMDD-HHMMSS>.changes
[--bwlimit <UP>:<DOWN>]                  limit_upload e limit_download (passo 2), omitida com os dois em off
--retries 1
--stats 2s --stats-log-level NOTICE
--use-json-log --log-file <state>/pairs/<id>/runs/<AAAAMMDD-HHMMSS>.jsonl -v
```

- Regra por trás da tabela: o que o lado **local** perde sempre vai para a pasta local de versions (todos os modos); o lado **remoto** só ganha uma pasta de versions no `mirror_up`, onde as exclusões são o ponto. No `bisync` e no `upload` o remoto continua apoiado na lixeira e nas revisões do Drive (ADR 0002).
- `RoundMode`: `Normal` roda com o teto `--max-delete <N>` (espelhos); `Resync` e `Force` rodam a revisão confirmada, sem teto; `DryRun(_)` acrescenta `--dry-run`, com `--log-file` e `--combined` em arquivos temporários ao lado (`preview-<stamp>.jsonl` e `.changes`, apagados com o guard que o `preview` já tem). Nos modos `copy` não há `--max-delete`.
- `<N>` do `--max-delete` (que no `sync` é contagem, ao contrário do bisync): `floor(dest_files * max_delete / 100)`, com `dest_files` lido de `state.json` (abaixo). `dest_files` ausente (nunca aconteceria com `applied_filters_hash` presente) vale 0: qualquer exclusão vira gate, que é o lado seguro.
- Pasta de versions remota do `mirror_up`: `<account>:.syncfast-versions/<id>/<stamp>`, na raiz da conta e fora da pasta do sync, como o rclone exige ("must not overlap the destination"). `<stamp>` é o mesmo `AAAAMMDD-HHMMSS` do local.
- Poda: `versions_keep_days` vale para as duas. A remota roda depois de cada rodada do `mirror_up`: `rclone lsf --dirs-only <account>:.syncfast-versions/<id>` e `rclone purge` de cada pasta datada mais velha que o limite (a idade vem do nome, nunca do `--min-age`, que olharia a data de modificação do arquivo guardado, não a do backup). Falha na poda é aviso, nunca falha da rodada (como `versions::prune`).

**Validação (erros por par):**

- `mirror_up with versions needs a folder on the account, not its root` quando `mode = mirror_up`, `versions` ligado e `remote` é a raiz da conta (`gdrive:`).
- Sobreposição remota: um `mirror_up` com `versions` ocupa também `<account>:.syncfast-versions`; outro sync que a sobreponha (inclui um na raiz da conta) dá `remote folder overlaps "<outro name>"`, a mensagem que já existe.
- `unknown value "<v>" for mode: use bisync, upload, download, mirror_up or mirror_down`.
- A pasta local precisa existir em todos os modos (`local_missing`), inclusive `download` e `mirror_down`: um disco desmontado não pode virar destino.

**Aplicabilidade das chaves por modo** (`applies_to`, usado por validação, GUI e `settings --json`; chave que não vale é aceita e ignorada, para que `[defaults]` e a troca de modo não percam configuração):

| Chave | `bisync` | `upload` | `download` | `mirror_up` | `mirror_down` |
|---|---|---|---|---|---|
| `watch`, `debounce`, `max_wait` | sim | sim | não | sim | não |
| `interval` | sim | sim | sim | sim | sim |
| `conflict_resolve`, `conflict_loser` | sim | não | não | não | não |
| `versions`, `versions_keep_days` | sim (local) | não | sim (local) | sim (remoto) | sim (local) |
| `max_delete` | sim | não (nunca apaga) | não | sim | sim |
| `resync_mode` | sim | não | não | não | não |
| `empty_dirs`, `review` | sim | sim | sim | sim | sim |
| `limit_upload` | sim | sim | não | sim | não |
| `limit_download` | sim | não | sim | não | sim |

`resync_mode` só vale no `bisync`: primeira rodada e volta de outro modo. `mode` não se aplica a si mesmo.

**Estado (`state.json`, campos novos com `serde(default)`):**

- `applied_mode: Option<SyncMode>`: o modo da última rodada que terminou `Ok`. Ausente num estado da fase 1: `Bisync` se `applied_filters_hash` existe, `None` se o sync nunca rodou.
- `dest_files: Option<u64>`: arquivos no destino depois da última rodada `Ok` de um espelho (`=`, `+` e `*` do `--combined`).
- Gate novo `GateKind::NeedsReviewModeChanged` (`needs_review_mode_changed`), contado em `is_needs_review()`; marcador `ReviewKind::Resync`.

**Decisão (`run_decision.rs`).** `DecideInput` ganha `mode` e `applied_mode`. Linhas novas da tabela:

- Depois das linhas de gate (`gate` pendente decide primeiro) e antes de `SetGate(NeedsReviewFiltersChanged)`: `applied_filters_hash` presente e `applied_mode != Some(mode)` dá `SetGate(NeedsReviewModeChanged)`. Se mudaram modo e filtros, ganha a revisão de modo (a confirmação cobre os dois).
- Um gate `NeedsReviewModeChanged` pendente cujo `mode` voltou a ser o `applied_mode` acaba sozinho (como `local_missing`): o gate sai e a chave sai de `notified`.
- `Decision::Run(Resync)` com marcador confirmado, em modo diferente do bisync, executa a revisão confirmada (`copy` ou `sync` sem teto); no bisync executa `--resync` como hoje.
- `discards_marker` não muda.

**Rodada (`run.rs`).** `Round::attempt` escolhe o motor pelo `behavior.mode` (`bisync_args` ou `oneway_args`). Para os modos `oneway`:

1. **Piso da fonte vazia**, antes de qualquer rclone e com qualquer `review`: `Rclone::has_files(origem, filters)` (`lsf -R --files-only --filter-from <f> <origem>`, lê o primeiro nome e encerra o processo). Origem sem arquivo: não roda nada; o gate vira `needs_attention` com `fault = LocalEmpty` (origem local) ou o novo `Fault::RemoteEmpty` (`remote_empty`, origem remota), `detail` `The source folder is empty, so nothing was changed.`, e a rodada conta como falha (`record_failure`). `attention_body` ganha `Some(Fault::RemoteEmpty) => "The folder on Google Drive is empty. Nothing was changed."`. Vale também para a rodada confirmada: o espelho nunca esvazia o destino. Origem que não pôde ser lida: erro transitório.
2. Roda o rclone, lê o `--combined` e o log.
3. **`classify_oneway(exit_code, lines)`**, na ordem: código 0 é `Ok`; erro de autenticação (`AuthError`); excesso de `--max-delete` (código e texto dos fixtures) é `TooManyDeletes`; falha que nomeia um `Fault` (`fault_of`) é `NeedsAttention`; código 2 é `ConfigError`; o resto é `Transient`. Não há `NeedsReviewCriticalError` nem `AllFilesChanged` nos modos de uma via (não há listings). **[ABERTO 2]**
4. **`summarize_oneway(changes_file, lines, mode)`**: cada linha do `--combined` vira uma `Change`: no `mirror_up`, `+` Upload, `*` OverwriteRemote, `-` DeleteRemote; no `mirror_down`, `+` Download, `*` OverwriteLocal, `-` DeleteLocal; no `upload` e no `download` só `+` e `*` (o `-` é arquivo que só existe no destino e o `copy` não toca nele). `=` não é mudança. `!` é erro, não mudança. Contadores só com código 0. Pastas não aparecem. Mesmo corte de 100 mudanças no `state.json` (ADR 0005).
5. Rodada `Ok` de um espelho grava `dest_files`. Toda rodada `Ok` grava `applied_mode` e `applied_filters_hash`; ao sair do `bisync` (o `applied_mode` anterior era `Bisync` e o novo não é) apaga `<state>/bisync/<id>/`.
6. Volta ao `bisync`: o `applied_mode` é outro, `decide` dá `NeedsReviewModeChanged`, a confirmação roda `--resync --resync-mode <resync_mode>` (a pasta de listings está vazia, é a primeira rodada de novo). Como `--resync` só copia, a revisão dessa volta não tem exclusões e o nível `deletions` a confirma sozinho.
7. `conflicts_pending`: só no `bisync`; nos outros modos vale 0 e a aba `Conflicts` some.

**Revisão dos espelhos.** Mesmos gates e mesmo `auto_review` do passo 4: `NeedsReviewFirstRun` (nunca sincronizou), `NeedsReviewFiltersChanged`, `NeedsReviewModeChanged` e `TooManyDeletes`, que vem da classificação. A revisão é um dry run do mesmo motor (`oneway_args` em `DryRun`), a lista vem do `--combined`, os contadores e `deletes()` são os de sempre e o texto de cada linha da sheet é o de sempre (`Would delete on <account> (n)`). Revisão confirmada de `TooManyDeletes` roda sem teto. O `ReviewView` ganha `mode` e, para `NeedsReviewModeChanged`, `reason = "Sync mode changed"`, explicação `Sync mode changed: review what will change before syncing resumes.` e notificação `<name>: review needed` / `Sync mode changed. Review what will change.`

**GUI.**

- Seção `Mode` só na aba `Options` (o painel `Defaults` não a tem: `mode` é por sync): combo `Mode` com `Two-way` (`bisync`), `Upload only`, `Download only`, `Mirror to Drive` (`mirror_up`), `Mirror to this computer` (`mirror_down`), e a legenda do modo escolhido:
  - Two-way: `Changes on either side reach the other. Deleted files are deleted on both sides.`
  - Upload only: `New and changed files on this computer are copied to Drive. Nothing is ever deleted.`
  - Download only: `New and changed files on Drive are copied to this computer. Nothing is ever deleted. Replaced files are kept as versions.`
  - Mirror to Drive, em cor `warning` com `triangle-alert`: `Drive becomes an exact copy of this folder. Files that exist only on Drive are deleted there and kept as versions for 30 days. An empty folder is never mirrored.`
  - Mirror to this computer, igual: `This folder becomes an exact copy of Drive. Files that exist only on this computer are deleted and kept as versions for 30 days. An empty folder on Drive is never mirrored.`
- Ao mudar o modo de um sync com histórico: nota em linha `Changing the mode holds this sync for a review.`; o resto é pelo gate. Sem `alert` modal (ADR 0004).
- Detalhe e lista: chip com o nome do modo quando não é `Two-way`. `Add Sync` e `add` criam sempre um sync `Two-way`; o modo muda depois, na aba `Options`, o único lugar onde se escolhe um espelho.
- A sheet de Review nomeia a direção (`Would delete on <account> (n)` no `mirror_up`, `Would delete on this computer (n)` no `mirror_down`) e o texto do botão de confirmação continua `Delete and Sync` quando há exclusões.
- Atualizar o mock.

**Agentes.** A guarda do passo 1 recusa `settings set mode mirror_up|mirror_down --sync <id>` (exit 4), inclusive para inverter um espelho. Como `mode` só existe por sync, `settings reset mode` volta ao `bisync` e passa. Mudar de `bisync` para `upload` ou `download` é permitido (não apaga, e passa pela revisão de modo). `settings set mode ...` sem `--sync` dá exit 2 com `mode is set per sync, not in [defaults]`. `Ops::confirm_review` continua recusando revisão com exclusões, em qualquer modo. `docs/agents.md` lista os modos e a regra.

**ADR 0008** registra o desenho (modos, ferramentas, regra do que cada lado perde, piso, revisão de mudança de modo).

**Testes:**

- `oneway_args`: snapshot do vetor por modo e por `RoundMode` (com e sem `--max-delete`, com `versions = false`, com `empty_dirs = false`).
- `classify_oneway` e `summarize_oneway` contra os fixtures `oneway_*` (um por ramo).
- `decide`: uma linha por regra nova (`a_changed_mode_of_a_synced_pair_sets_needs_review_mode_changed`, `the_mode_gate_ends_when_the_mode_is_back`, `a_confirmed_mode_review_runs_as_resync`).
- Integração local↔local (`tests/oneway_local.rs`):
  - `upload` envia e nunca apaga um arquivo só do destino; sobrescreve o alterado;
  - `download` baixa, o local substituído vai para a pasta de versions;
  - `mirror_up` apaga no destino o que não existe na origem e o apagado fica na versions remota (`.syncfast-versions/<id>/<stamp>`); nada fora da pasta do sync é tocado;
  - `mirror_down` idem no local, com versions local;
  - origem vazia nunca esvazia o destino em nenhum `review` (`an_empty_mirror_source_needs_attention_and_the_destination_keeps_every_file`);
  - mais exclusões que `max_delete` (teto do `--max-delete`) param em `too_many_deletes`; a revisão confirmada roda;
  - primeira rodada de espelho fica em revisão em `manual` e em `deletions`, com ou sem exclusões, e confirma sozinha em `automatic`;
  - mudar de `bisync` para `upload`: gate de modo, depois da confirmação os listings somem; voltar: gate de modo, `--resync` refaz os listings e o próximo `bisync` normal roda (`leaving_and_returning_to_bisync_rebuilds_the_listings`);
  - poda das versions remotas por idade do nome da pasta.
- Config: `mirror_up_with_versions_needs_a_folder_not_the_account_root`, `a_mirror_up_sync_occupies_the_versions_folder_for_overlap`, `unknown_mode_value`, `mode_in_defaults_is_rejected_with_a_clear_message`.
- Ops: `an_agent_cannot_switch_a_sync_to_a_mirror_mode`, `an_agent_cannot_flip_a_mirror_direction`, `an_agent_can_switch_to_upload`.
- GUI: combo e legendas só na aba `Options` (ausente no painel `Defaults`), chip, `Add Sync` sempre `Two-way`, Review com texto de direção, aba `Conflicts` ausente fora do `bisync`, campos de watch ocultos em `download` e `mirror_down`.
- Se o passo 4 já entrou, o teste cruzado `a_mode_change_waits_for_review_at_manual_and_confirms_at_deletions_when_nothing_is_deleted`; se não, o passo 4 o acrescenta.

`just check` verde.

### 7. Spike: Google Docs

Arquivos: `tests/capture_gdocs.sh`, relatório e ADR.

Independente. O `GoogleDocs { Skip, Export, Url }` já existe e continua; nada do runtime muda neste passo.

Fatos de partida (docs do rclone, v1.66 em diante): o bisync suporta Google Docs com as opções do `sync`; no sentido Drive para disco vale `--drive-export-formats` (padrão `docx,xlsx,pptx,svg`, ou `url` para atalhos); no sentido disco para Drive vale `--drive-import-formats` (sem padrão). Todo Google Doc no Drive tem tamanho -1 e nenhum checksum. O dono observou que a edição local de um `docx` exportado volta como arquivo separado quando `--drive-import-formats` não está definido. A doc diz, ao contrário, que um `.xlsx` com o nome de uma planilha existente é sincronizado com essa planilha. O spike decide qual é verdade na versão do rclone embutida.

Entregáveis:

- `crates/syncfast-core/tests/capture_gdocs.sh <remote> <pasta>`: roda numa conta real (a do dono) numa pasta de teste, **nunca** nos testes automáticos (`AGENTS.md`: testes de integração só com remotes `type = local`). Cria os Docs com `rclone copy --drive-import-formats docx,xlsx ...` a partir de arquivos locais (confirmar que a conversão cria um Google Doc), roda `rclone bisync` com as opções de cada caso e grava, em `crates/syncfast-core/tests/fixtures/rclone-1.75.2/gdocs/`, o log JSON, o código de saída e a listagem dos dois lados (`lsjson`) de cada cenário. Esses arquivos são texto e são commitados.
- Cenários (cada um, com `--drive-export-formats docx,xlsx,pptx,svg` e depois com `url`):

  | Cenário | Pergunta |
  |---|---|
  | `export_first_sync` | o que aparece no disco, com que tamanho e data, e o que o `--resync --dry-run` lista |
  | `export_edit_local` | editar o `docx` exportado: vira o mesmo Doc, um arquivo separado ou erro? com e sem `--drive-import-formats docx` |
  | `export_edit_remote` | editar o Doc no Drive: o bisync baixa de novo? |
  | `export_delete_local` | apagar o arquivo exportado: o Doc vai para a lixeira do Drive? |
  | `url_edit_local` | editar o `.url` local: a doc diz que dá erro |
  | `url_delete_local` | apagar o `.url` apaga o Doc (a doc diz que sim) |
  | `rename_remote` | renomear o Doc no Drive: apaga e recria no disco? |
  | `with_max_delete_and_backup_dir1` | `--max-delete` e `--backup-dir1` contam e guardam os Docs? |
  | `skip_baseline` | `--drive-skip-gdocs` continua igual ao de hoje |

- `docs/design/gdocs-spike.md` (português, com a tabela dos resultados, citando o fixture de cada linha) e a decisão. **Critério objetivo** (decidir a partir dos fixtures, não da doc):
  - Habilitar `google_docs = export` (e só ele) numa fase seguinte **se** nenhum cenário perde dado (um Doc só some por exclusão explícita do usuário e vai para a lixeira) **e** uma edição local de um arquivo exportado nunca cria uma cópia divergente que o bisync depois replica como arquivo duplicado (ou o resultado é estável e explicável na UI) **e** os gates (`--max-delete`, revisão, `TooManyDeletes`) contam os Docs.
  - `url` só entra se o `url_delete_local` não apagar o Doc sem a revisão contar como exclusão.
  - Caso contrário, `skip` fica e a decisão é registrada como rejeição.
  - Em qualquer caso, escrever um ADR curto em inglês (`docs/adr/0010-google-docs.md`; o 0009 é do backup da fase 3) com o resultado e a decisão, e, se habilitar, abrir a issue da chave `google_docs` com as semânticas medidas (fora desta fase).
- Sem mudança em `just check` (nenhum teste novo roda contra a Drive). Os fixtures entram só como texto; se o spike habilitar a chave, os parsers dela serão escritos contra eles na issue seguinte.

**Verificação do passo:** o relatório cita cada fixture; a decisão aponta qual item do critério a sustenta; `just check` verde.

## Divisão em issues

Uma issue por grupo, cada uma entregável sozinha (core, CLI/MCP e GUI juntos quando há tela). Os corpos estão em `local://issues-phase-2.json` e referenciam as seções abaixo.

| Issue | Título | Passo | Depende de |
|---|---|---|---|
| P2.1 | Config v2, `[defaults]`, inheritance and settings commands | 1 | nenhuma |
| P2.2 | Schedule, watch and bandwidth options | 2 | P2.1 |
| P2.3 | Conflict and version options | 3 | P2.1 |
| P2.4 | Review levels | 4 | P2.1 |
| P2.5 | Notification and app options | 5 | P2.1 |
| P2.6 | Sync modes | 6 | P2.1 |
| P2.7 | Spike: Google Docs in bisync | 7 | nenhuma |

P2.2 a P2.6 são independentes entre si. Os pontos onde duas se encontram (revisão de mudança de modo em P2.4 e P2.6; `watch` por modo em P2.2 e P2.6; notificação de revisão automática em P2.4 e P2.5) têm o teste cruzado descrito no passo de quem entrar depois.

Processo: o mesmo fluxo de fatia da fase 1 (branch por issue, `just check`, revisão por agente independente, PR com `Closes #<issue>`, squash). P2.1 para num marco: a PR é aberta e espera o usuário olhar a mudança de schema antes do merge.

## Arquivos críticos

- `crates/syncfast-core/src/settings.rs` (novo): o registro de chaves, de onde saem validação, `lowers_safety` e `applies_to`.
- `crates/syncfast-core/src/config.rs`: `Config::parse`, `reject_unknown_keys`, `migrate`, `set_setting`, `reset_setting`.
- `crates/syncfast-core/src/behavior.rs` `Behavior::for_pair`: a resolução; todo consumidor passa por ela.
- `crates/syncfast-core/src/ops/settings.rs` (novo): a guarda dos agentes.
- `crates/syncfast-core/src/run_decision.rs` `decide` e `auto_review`: a tabela de revisão com níveis e a linha de mudança de modo.
- `crates/syncfast-core/src/oneway.rs` (novo): argumentos, `classify_oneway` e `summarize_oneway` dos modos de uma via, travados pelos fixtures `oneway_*`.
- `docs/adr/0002-bisync-safety-invariants.md`, `docs/adr/0008-sync-modes.md`: as invariantes de segurança.

## Verificação

Diretório `~/Desktop/Personal/syncfast`. Depois que todas as issues entraram:

1. `just check` verde (inclui os testes novos de cada passo e os de UI).
2. `just mutants` sem sobrevivente em `settings.rs`, `run_decision.rs`, `oneway.rs` e na guarda de `ops/settings.rs`.
3. Roteiro dev (sem Drive e sem systemd), com `just dev-reset && just run-gui-dev`:
   1. Config antigo: antes de abrir, `.dev/config/syncfast/config.toml` com `version = 1` e um sync. Ao abrir, `config.toml.v1` existe e o arquivo diz `version = 2`.
   2. `just run-cli-dev settings`: lista as chaves (sem `mode`) com `origin = builtin`. `just run-cli-dev settings set debounce 10s` e `settings set debounce 2s` (a segunda sai com exit 2 e `debounce must be at least 5s`). `settings set limit_upload 2m` grava `2M` em `[defaults]` e `settings set limit_upload 0` sai com exit 2; `settings set mode upload` sem `--sync` sai com exit 2 e `mode is set per sync, not in [defaults]`.
   3. Settings › Defaults mostra `Wait after changes` com `10 s` alterado e `Built-in default: 30 s`. Na aba `Options` de um sync, o campo herda: `Default: 10 s`. Mudar o campo para `20s` mostra `Overrides the default (10 s)`; `Reset to Default` volta.
   4. `just run-cli-dev settings set max_delete 80 --sync dev` sai com exit 4 e `This setting lowers safety (max_delete). Change it in syncfast.`; `settings set max_delete 30 --sync dev` passa. Na GUI, `max_delete` 80 grava e aparece o selo `Safety lowered`.
   5. Revisão: `Create Sync` de `devremote` com a pasta local cheia e o remoto vazio abre a sheet de Review mesmo sem nada a apagar (nível `deletions`: a primeira rodada sempre espera); confirmar leva a `Up to date`. `just run-cli-dev settings set review automatic` sai com exit 4. Pela GUI, Settings › Defaults em `Never ask`: remover o sync e criá-lo de novo vai direto a `Syncing…` sem sheet e a Activity mostra `Reviewed automatically`. Com o sync em `Never ask`, criar 4 arquivos, sincronizar e apagar 3 sincroniza as exclusões sem sheet. Voltar a `Ask every time` com `just run-cli-dev settings set review manual` (exit 0, é mais seguro) para o passo seguinte.
   6. Modos, com o sync em `Ask every time`: pela aba `Options` pôr `Upload only`: gate `Review needed` (`Sync mode changed`), `Delete and Sync` não aparece (nada apaga); apagar um arquivo local não o apaga no remoto. Pôr `Mirror to Drive`: o gate volta; a sheet mostra `Would delete on devremote (n)` para o que só existe no remoto; `settings set mode mirror_up --sync dev` pela CLI sai com exit 4. Esvaziar a pasta local: `Needs attention` com `The source folder is empty, so nothing was changed.` e o remoto intacto. Voltar a `Two-way`: gate, `--resync`, `Up to date`.
   7. Conflitos: `conflict_loser pathname`: alterar o mesmo arquivo nos dois lados gera `.path1` e `.path2` e a aba Conflicts os lista. `delete` mostra o aviso e o estado vazio da aba.
   8. Notificações: com `notifications.needs_action` desligado (`settings set notifications.needs_action false`), mudar os filtros do sync para forçar `Review needed`: o gate aparece na GUI e nenhuma notificação do desktop sai. Com `notifications.success` ligado, uma rodada com mudança notifica `<name>: synced`.
4. Drive real:
   1. `mirror_up` num sync de teste (`gdrive:syncfast-e2e`, `~/syncfast-e2e`): `rclone lsf gdrive:.syncfast-versions` lista a pasta do sync depois que um arquivo foi apagado localmente e a rodada correu.
   2. Autostart: o teste manual em GNOME ou KDE (fora desta máquina) com `Start syncfast when you log in` confirma a entrada em `~/.config/autostart` e a abertura oculta no próximo login. Nesta máquina a legenda do Hyprland aparece.
   3. Banda: `mirror_up` ou `bisync` de um arquivo de 4 MiB em `gdrive:syncfast-e2e` com `settings set limit_upload 1M --sync dev` leva ao menos uns 4 s para enviar, e com só `limit_download 1M` o mesmo envio não é limitado (fecha o item 5 dos em aberto).
   4. Spike (P2.7): `tests/capture_gdocs.sh gdrive syncfast-gdocs-spike`, depois `rclone purge gdrive:syncfast-gdocs-spike` e `rclone purge gdrive:.syncfast-versions`.

## Itens em aberto

Marcados como `[ABERTO n]` no texto. Nenhum bloqueia o início; cada um tem o teste que o fecha.

1. **Reinício do timer:** se o `daemon-reload` sozinho já aplica um `interval` novo a um timer em execução. Fecha no passo 2 com uma unit real.
2. **Fixtures dos modos de uma via:** `--combined` sob `--dry-run`, efeito de `--retries 1`, código de saída e texto do `--max-delete` excedido, e o encerramento do `lsf` depois do primeiro nome. Fecham no passo 6, antes dos parsers.
3. **Nomes do `conflict_loser = pathname`:** `file.txt.path1` e `.path2` com `--conflict-suffix path` vêm da doc. Fecha com os fixtures do passo 3.
4. **Autostart fora do Hyprland:** o comportamento do portal de segundo plano em GNOME e KDE não é verificável aqui. Fecha com o teste manual da Verificação.
5. **Direção do `--bwlimit UP:DOWN`:** em cópia local para local o rclone não aplica o limite direcional (verificado), então a direção só se confirma com um remote de verdade. Fecha com o teste da Verificação (Drive real, item 4.3): um envio de 4 MiB com `limit_upload 1M` leva ao menos uns 4 s e com só `limit_download 1M` não é limitado. Os testes automáticos cobrem só os argumentos (`--bwlimit 2M:off`).

Decisões que a spec tomou por conta própria (o dono pode reverter): `max_wait` no mínimo igual a `debounce`; limites `versions_keep_days` em `1..=3650` e `failures_after` em `0..=100`; combinação `conflict_resolve = none` com `conflict_loser = delete` recusada; nome `.syncfast-versions` para a pasta de versions remota e a regra de que `mirror_up` com versions exige uma pasta da conta; `Upload only` sem pasta de versions remota (o Drive guarda revisões); limites de banda só em inteiros com `K`, `M` ou `G`, com `off` no lugar do `0` do rclone.
