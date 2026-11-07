# Spike: Google Docs no bisync

Relatório da issue #55 (passo 7 de `docs/design/spec-phase-2.md`). Decisão registrada em `docs/adr/0010-google-docs.md`.

## Como foi medido

- rclone **v1.75.2** (o embutido no Flatpak e no CI; a doc do bisync diz que Docs são suportados desde a v1.66).
- `crates/syncfast-core/tests/capture_gdocs.sh Raphael syncfast-gdocs-spike` rodou contra o Drive real do dono, numa pasta que o script só cria se ela não existir e apaga ao sair (`rclone purge --drive-use-trash=false`). Nada fora dela foi tocado. Conferido no fim: `rclone lsf Raphael: --dirs-only | grep -i syncfast` sem saída.
- Cada cenário tem a sua subpasta no Drive, com 4 itens: três Docs criados por `rclone copy --drive-import-formats docx,xlsx` (`notes.docx` e `plan.docx` viram Google Docs, `budget.xlsx` vira Google Sheets; a conversão foi confirmada: três itens com tamanho -1) e um `plain.txt` comum de controle. Não há Slides nem Drawings: não existe como criá-los por importação de arquivo simples; usam o mesmo caminho de exportação.
- Os flags do bisync são os do app (`--resilient --recover --max-lock 2m --conflict-resolve newer --conflict-loser num --create-empty-src-dirs`), sem `--drive-skip-gdocs`. `--max-delete` e `--backup-dir1` só entram nos cenários que os estudam. O modo é `--drive-export-formats docx,xlsx,pptx,svg` (`export`) ou `--drive-export-formats url` (`url`).
- Fixtures em `crates/syncfast-core/tests/fixtures/rclone-1.75.2/gdocs/`, texto. Por cenário `<nome>`:
  - `<nome>.jsonl`: log JSON do bisync;
  - `<nome>.exit`: código de saída;
  - `<nome>.flags`: os flags da rodada;
  - `<nome>.local_before.json` e `<nome>.remote_before.json`: `lsjson` dos dois lados antes da rodada;
  - `<nome>.local.json`, `<nome>.remote.json`: `lsjson` depois;
  - `<nome>.trash.json`: itens na lixeira do Drive dentro da pasta do cenário (`--drive-trashed-only`);
  - `<nome>.backup.txt`: arquivos que o `--backup-dir1` guardou (só nos cenários dele).
- Sufixos: `_dry_run` (com `--dry-run`), `_again` (a mesma rodada de novo, para ver se o resultado é estável), `_recover` (tentativa de recuperar com `--resync --resync-mode newer`).

## Resultados

Todos os nomes abaixo são prefixos de arquivo em `fixtures/rclone-1.75.2/gdocs/`.

| Cenário | Fixture | Saída | O que aconteceu |
|---|---|---|---|
| Primeira rodada, `export` | `export_first_sync_dry_run`, `export_first_sync` | 0, 0 | O dry run lista `Skipped copy` dos três Docs sem tamanho (rclone não sabe) e do `plain.txt` com `(size 9)`. A rodada real grava `notes.docx` 6483 B, `plan.docx` 6482 B, `budget.xlsx` 7855 B; o mtime do arquivo local é o do Doc. No Drive o tamanho segue -1. |
| Primeira rodada, `url` | `url_first_sync_dry_run`, `url_first_sync` | 0, 0 | Cada Doc vira um `.url` de 123 a 127 B. |
| Sem mudança | `export_no_changes`, `url_no_changes` | 0, 0 | `No changes found`: o export não é baixado de novo a cada rodada. |
| Editar o arquivo exportado, sem `--drive-import-formats` | `export_edit_local_no_import`, `export_edit_local_no_import_recover` | 7, 7 | Não vira arquivo separado: `Failed to copy: can't update google document type without --drive-import-formats`, três retentativas e `Bisync critical error ... Must run --resync`. O Doc fica intacto. `--resync` não recupera enquanto a edição local existir: falha com o mesmo erro. |
| Editar o `docx` exportado, com `--drive-import-formats docx` | `export_edit_local_import`, `export_edit_local_import_again` | 0, 0 | O Doc é atualizado no lugar (`Copied (replaced existing)`; o ID do Doc em `remote_before.json` e `remote.json` é o mesmo). A segunda rodada não muda nada. |
| Editar o `xlsx` exportado, com `docx,xlsx` | `export_edit_local_import_xlsx`, `export_edit_local_import_xlsx_again` | 0, 0 | Idem: mesmo ID, estável. |
| Editar o `.url` local | `url_edit_local`, `url_edit_local_recover` | 7, 7 | `cannot update link files`, critical, e o `--resync` também falha. |
| Editar o Doc no Drive | `export_edit_remote`, `export_edit_remote_again`; `url_edit_remote`, `url_edit_remote_again` | 0 em todas | O export é baixado de novo (`Copied (Rcat, replaced existing)`, 6483 B para 6491 B); o `.url` também é substituído. A segunda rodada não muda nada. |
| Editar os dois lados, com `--drive-import-formats docx` | `export_edit_both`, `export_edit_both_again` | 0, 0 | O Doc antigo vai para a **lixeira** e nasce um Doc novo com **outro ID**; a cópia perdedora vira `notes.docx.conflict1`, um arquivo comum (não é Doc) que o bisync replica nos dois lados. A rodada seguinte ainda baixa o novo Doc por cima do arquivo local (882 B para 6492 B). |
| Editar os dois lados, sem import | `export_edit_both_no_import` | 0 | O Doc vai para a lixeira e é **substituído por um arquivo `notes.docx` comum de 882 B** (não é mais Doc), mais o `notes.docx.conflict1` comum. |
| `docx` novo criado no disco, sem import | `export_new_local_no_import`, `export_new_local_no_import_again` | 0, 0 | Vai para o Drive como arquivo comum (879 B, tamanho real), estável. |
| `docx` novo criado no disco, com import | `export_new_local_import`, `export_new_local_import_again` | 0, 0 | Vira um Doc; na rodada seguinte o export dele **substitui o arquivo do usuário** (879 B para 6486 B). |
| Apagar o arquivo exportado | `export_delete_local` | 0 | `Queue delete` no Drive: o Doc vai para a **lixeira do Drive** (`export_delete_local.trash.json` o lista). |
| Apagar o `.url` | `url_delete_local` | 0 | Idem: o log conta `1 deleted`, `Queue delete`, e o Doc vai para a lixeira. |
| Renomear o Doc no Drive | `export_rename_remote`, `export_rename_remote_again`; `url_rename_remote`, `url_rename_remote_again` | 0 em todas | Como com qualquer arquivo: `1 new, 1 deleted`; o arquivo novo é baixado e o antigo apagado no disco. Nada vai para a lixeira, nada se perde, estável. |
| `--max-delete 50` e `--backup-dir1`, acima do teto | `export_with_max_delete_and_backup_dir1_over_cap`, `url_with_max_delete_and_backup_dir1_over_cap` | 1, 1 | Apagar três dos quatro arquivos: `too many deletes (>50%, 3 of 4)`. Os Docs **são contados** e nada é apagado no Drive. |
| `--max-delete 50` e `--backup-dir1`, abaixo do teto | `export_with_max_delete_and_backup_dir1_under_cap`, `url_with_max_delete_and_backup_dir1_under_cap` | 0, 0 | O Doc apagado no Drive tem o arquivo local movido para o `--backup-dir1` (`notes.docx` e `notes.url` em `*.backup.txt`). O Doc em si fica na lixeira do Drive. |
| `--drive-skip-gdocs` | `skip_baseline` | 0 | Os Docs não aparecem em nenhum lado e o bisync só enxerga `plain.txt` e o arquivo novo. Igual ao comportamento atual. |

Observações que valem para a decisão:

1. A observação do dono ("a edição local volta como arquivo separado") não se reproduziu como descrita na v1.75.2. Sem `--drive-import-formats` a edição vira erro crítico (`export_edit_local_no_import`), nunca um arquivo separado. Com o formato certo (`docx`, `xlsx`) o mesmo Doc é atualizado, como diz a doc do rclone. O arquivo separado só aparece num conflito (`.conflict1`) ou, sem import, quando o arquivo local não corresponde a nenhum Doc.
2. O erro crítico sem import é um beco: o app pediria `--resync`, e o `--resync` falha de novo com a edição presente. O usuário teria de reverter o arquivo.
3. Cada formato precisa de import próprio (`docx` não cobre `xlsx`). Editar uma planilha exportada com `docx` apenas repete o erro de `export_edit_local_no_import`.

## Decisão

Aplicando o critério da spec aos fixtures:

- **`export`: rejeitado.**
  - Critério 1 (nenhum cenário perde dado: um Doc só some por exclusão explícita, para a lixeira) falha em `export_edit_both` e `export_edit_both_no_import`. Uma edição nos dois lados manda o Doc para a lixeira sem exclusão do usuário e cria outro no lugar, com outro ID (links, compartilhamento e histórico se perdem), ou nem isso: sem import o Doc é trocado por um arquivo comum. O conteúdo sobrevive (na lixeira e no `.conflict1`), mas o Doc, como objeto, não.
  - Critério 2 (uma edição local nunca cria uma cópia divergente que o bisync replica como duplicata, ou o resultado é estável e explicável na UI) falha: o `.conflict1` é um arquivo comum replicado nos dois lados, e `export_new_local_import_again` mostra o export do Drive reescrevendo o arquivo do usuário. Além disso, sem import (o padrão do rclone) a primeira edição local trava o par (`export_edit_local_no_import_recover`), e o import precisa ser configurado formato a formato.
  - Critério 3 (os portões contam os Docs) é atendido: `export_with_max_delete_and_backup_dir1_over_cap` e `export_delete_local` mostram `--max-delete` e a contagem de exclusões incluindo os Docs.
  - Um critério que falha basta: `google_docs = export` não entra.
- **`url`: não entra agora.** A única condição de entrada da spec (`url_delete_local` não apagar o Doc sem a revisão contar a exclusão) é atendida: o log conta `1 deleted` e `url_with_max_delete_and_backup_dir1_over_cap` para no teto. Mas apagar um atalho apaga o Doc de verdade, e a edição local do `.url` trava o par sem saída (`url_edit_local_recover`). Como não existe nenhuma tarefa do produto que peça atalhos para Docs, a chave não vale o custo agora. A condição fica registrada para quem reabrir o assunto.
- **`skip` continua como está.** `GoogleDocs { Skip, Export, Url }` e `--drive-skip-gdocs` ficam sem mudança; nenhuma issue nova é aberta.

## Limites da medição

- Só Docs e Sheets foram medidos; Slides (`pptx`) e Drawings (`svg`) usam o mesmo mecanismo de export, sem fixture próprio.
- O round trip `docx` para Doc para `docx` pode perder formatação; os fixtures usam um documento mínimo e não medem isso.
- rclone v1.75.2; uma versão nova pode mudar mensagens e comportamentos. Para remedir, rode o script de novo (ele reescreve o diretório de fixtures da versão instalada).
