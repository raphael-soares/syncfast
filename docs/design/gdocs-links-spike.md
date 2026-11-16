# Spike: Google Docs como arquivos de link no rclone 1.76.0

Relatório da issue #125. Decisão registrada em `docs/adr/0010-google-docs.md`; o spike anterior (1.75.2) está em `docs/design/gdocs-spike.md`.

## Como foi medido

- rclone **v1.76.0**, a mesma do Flatpak.
- `crates/syncfast-core/tests/capture_gdocs.sh Raphael syncfast-gdocs-spike` rodou contra o Drive real, numa pasta de teste que o script apaga ao sair. Conferido no fim: `rclone lsf Raphael: --dirs-only | grep -i syncfast` sem saída. 98 rodadas, todas terminaram.
- Os flags do bisync são os do app hoje: `--resilient --recover --max-lock 2m --conflict-resolve newer --conflict-loser num --create-empty-src-dirs --drive-use-trash`, mais `--track-renames --track-renames-strategy hash,modtime --max-delete-renames-aware` fora de resync. Sem `--drive-skip-gdocs`, exceto nos cenários `skip_*`.
- Formatos: `export` (`docx,xlsx,pptx,svg`) e os três formatos de link do rclone, `desktop`, `url` e `link.html` (prefixo `link_html` nas fixtures).
- Cada cenário tem três Docs (`notes`, `plan`, `budget`) e um `plain.txt` de controle. O cenário da pasta só com Doc acrescenta `only-docs/only`.
- Fixtures em `crates/syncfast-core/tests/fixtures/rclone-1.76.0/gdocs/`, com os mesmos arquivos por cenário do spike anterior (`.jsonl`, `.exit`, `.flags`, `.local_before.json`, `.remote_before.json`, `.local.json`, `.remote.json`, `.trash.json`, `.backup.txt`). Sufixos novos: `_heal` (apaga o link mexido e faz resync) e `_heal_again`.

## Resultados

Os três formatos de link se comportam igual em todo cenário de bisync; muda só o arquivo (`desktop` 176 a 185 B, `url` 123 a 127 B, `link.html` 341 a 353 B). `<f>` vale `desktop`, `url` ou `link_html`.

| Cenário | Fixture | Saída | O que aconteceu |
|---|---|---|---|
| Pasta só com Doc apagada no disco, skip (o bug) | `skip_delete_local_folder_only_docs`, `_again` | 0, 0 | O rmdir no Drive falha calado; a rodada seguinte loga `File is new - only-docs` e recria a pasta no disco. |
| Pasta só com Doc apagada no disco, link | `<f>_delete_local_folder_only_docs`, `_again` | 0, 0 | Doc e pasta vão para a lixeira; a pasta não volta. |
| Apagar o link | `<f>_delete_local` | 0 | O Doc vai para a lixeira do Drive. |
| Apagar muitos links | `<f>_with_max_delete_and_backup_dir1_over_cap` | 1 | `too many deletes (>50%, 3 of 4)`, nada apagado no Drive. |
| Doc apagado no Drive com backup | `<f>_with_max_delete_and_backup_dir1_under_cap` | 0 | O link local vai para o `--backup-dir1`. |
| Sem mudança, edição e renomeação no Drive | `<f>_no_changes`, `<f>_edit_remote_again`, `<f>_rename_remote_again` | 0 | Estável. |
| **Renomear o link no disco** | `<f>_rename_local`, `_again` | 0, 0 | O Doc original vai para a **lixeira** e sobe no lugar um arquivo de link comum, com outro ID. Nenhum erro. |
| **Mover o link no disco** | `<f>_move_local`, `_again` | 0, 0 | Idem. `--track-renames` não ajuda: o Doc não tem hash. |
| **Editar o link** | `<f>_edit_local`, `_recover`, `_heal`, `_heal_again` | 7, 7, 0, 0 | `Failed to copy: cannot update link files`, depois `Bisync critical error` e `Must run --resync`. O resync falha igual; só destrava apagando o link (`_heal`), e o Doc fica com o mesmo ID. |
| Copiar ou criar link no disco | `<f>_copy_local`, `<f>_new_local_link` | 0 | Sobe para o Drive um arquivo de link comum, que não é Doc. |
| `export` | `export_*` | | Repete o 1.75.2: conflito manda o Doc para a lixeira e cria outro ID (`export_edit_both`), editar sem import trava o par (`export_edit_local_no_import_recover`), e `export_edit_both_again` e `export_new_local_import_again` ainda baixam o export por cima do arquivo local. |

## Abertura no Nautilus

Arquivos gerados pelo rclone para um Doc real, aberto pelo dono com duplo clique no Nautilus 51.1:

- `.desktop`: abre como texto (o tipo `application/x-desktop` vai para o editor; o Nautilus não executa lançadores). Modo 0644, sem `metadata::trusted`.
- `.url`: abre como texto (`application/x-mswinurl`).
- `.link.html`: abre o Doc no navegador (`text/html` vai para o navegador, e a página redireciona para o Doc).

Dolphin não foi medido.

## Conclusão

Nenhum formato do rclone serve sozinho. O bisync identifica arquivo por caminho, então renomear ou mover um link vira apagar o Doc e criar um arquivo comum. Clientes que mostram Docs no disco, como o Google Drive for desktop e o Insync, guardam um ponteiro com o ID do Doc (`.gdoc`, `.gddoc`) e são clientes próprios, não motores por caminho. O caminho escolhido no ADR 0010 faz o mesmo no syncfast: links próprios `<nome>.<ext>.html` (`Ata.gdoc.html`, `Planilha.gsheet.html`) mantidos pelo app, por ID, fora do bisync.

## Limites

- Só Docs e Sheets; Slides e Drawings não se criam por importação, mas usam o mesmo caminho de exportação.
- Um Drive, uma máquina, Nautilus. Dolphin e outros gerenciadores não foram medidos.
- O comportamento pode mudar com o rclone; `capture_gdocs.sh` remede.
