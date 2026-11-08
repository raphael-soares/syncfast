use std::collections::HashSet;

use egui::{Key, Ui};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::state::DisplayStatus;
use syncfast_gui::icons::Icon;
use syncfast_gui::theme::{self, Palette, ThemeChoice, Themes};
use syncfast_gui::ui::widgets::{
    AlertButton, BadgeSize, FolderNode, FolderTree, NodeState, PrimaryKind, SheetAction,
    SheetButtons, SidebarRow, TreeAction, alert, destructive_button, folder_tree, icon_button,
    inline_error, progress_bar, segmented, setting_row, sheet, sidebar_row, status_badge, switch,
    text_field,
};

/// Installs the theme, and tells whether fonts are usable: `set_fonts` takes
/// effect on the next pass, so the first one must not draw.
fn ready(ui: &Ui) -> bool {
    theme::install_with(
        ui.ctx(),
        &Palette::dark(),
        fastframe_text::TextRendering::platform_default(),
    );
    ui.ctx().fonts(|fonts| {
        fonts
            .families()
            .contains(&egui::FontFamily::Name("inter-semibold".into()))
    })
}

fn harness<'a, S: 'a>(mut draw: impl FnMut(&mut Ui, &mut S) + 'a, state: S) -> Harness<'a, S> {
    let mut harness = Harness::new_ui_state(
        move |ui, state| {
            if ready(ui) {
                draw(ui, state);
            }
        },
        state,
    );
    harness.run_ok();
    harness
}

fn sheet_harness<'a>(buttons: SheetButtons<'static>) -> Harness<'a, Option<SheetAction>> {
    harness(
        move |ui, chosen: &mut Option<SheetAction>| {
            if let Some(action) = sheet(ui.ctx(), "test", "Review Changes", &buttons, |ui| {
                ui.label("Body text");
            }) {
                *chosen = Some(action);
            }
        },
        None,
    )
}

#[test]
fn sheet_enter_triggers_the_primary_button() {
    let mut harness = sheet_harness(SheetButtons::new("Sync"));
    harness.run();

    harness.key_press(Key::Enter);
    harness.run();

    assert_eq!(*harness.state(), Some(SheetAction::Primary));
}

#[test]
fn sheet_escape_triggers_cancel() {
    let mut harness = sheet_harness(SheetButtons::new("Sync"));
    harness.run();

    harness.key_press(Key::Escape);
    harness.run();

    assert_eq!(*harness.state(), Some(SheetAction::Cancel));
}

#[test]
fn sheet_with_a_destructive_primary_cancels_on_enter() {
    let mut buttons = SheetButtons::new("Delete and Sync");
    buttons.primary_kind = PrimaryKind::Destructive;
    let mut harness = sheet_harness(buttons);
    harness.run();

    harness.key_press(Key::Enter);
    harness.run();

    assert_eq!(*harness.state(), Some(SheetAction::Cancel));
}

#[test]
fn sheet_with_a_destructive_primary_still_runs_it_on_click() {
    let mut buttons = SheetButtons::new("Delete and Sync");
    buttons.primary_kind = PrimaryKind::Destructive;
    let mut harness = sheet_harness(buttons);
    harness.run();

    harness.get_by_label("Delete and Sync").click();
    harness.run();

    assert_eq!(*harness.state(), Some(SheetAction::Primary));
}

#[test]
fn sheet_enter_does_nothing_while_the_primary_is_disabled() {
    let mut buttons = SheetButtons::new("Continue");
    buttons.primary_enabled = false;
    let mut harness = sheet_harness(buttons);
    harness.run();

    harness.key_press(Key::Enter);
    harness.run();

    assert_eq!(*harness.state(), None);
    assert!(
        harness
            .get_by_label("Continue")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn sheet_with_back_puts_cancel_left_of_back_and_the_primary() {
    let mut buttons = SheetButtons::new("Continue");
    buttons.back = true;
    let mut harness = sheet_harness(buttons);
    harness.run();

    let cancel = harness.get_by_label("Cancel").rect();
    let back = harness.get_by_label("Back").rect();
    let primary = harness.get_by_label("Continue").rect();

    assert!(cancel.right() < back.left(), "{cancel:?} {back:?}");
    assert!(back.right() < primary.left(), "{back:?} {primary:?}");
}

#[test]
fn sheet_with_back_and_no_cancel_has_back_and_the_primary_only() {
    let mut buttons = SheetButtons::new("Continue");
    buttons.back = true;
    buttons.cancel = false;
    let mut harness = sheet_harness(buttons);
    harness.run();

    assert!(harness.query_by_label("Cancel").is_none());
    harness.get_by_label("Back");
    harness.get_by_label("Continue");
}

#[test]
fn sheet_without_back_puts_cancel_left_of_the_primary() {
    let mut harness = sheet_harness(SheetButtons::new("Sync"));
    harness.run();

    let cancel = harness.get_by_label("Cancel").rect();
    let primary = harness.get_by_label("Sync").rect();

    assert!(cancel.right() < primary.left(), "{cancel:?} {primary:?}");
    assert!(harness.query_by_label("Back").is_none());
}

#[test]
fn sheet_back_button_reports_back() {
    let mut buttons = SheetButtons::new("Continue");
    buttons.back = true;
    let mut harness = sheet_harness(buttons);
    harness.run();

    harness.get_by_label("Back").click();
    harness.run();

    assert_eq!(*harness.state(), Some(SheetAction::Back));
}

fn remove_alert<'a>() -> Harness<'a, Option<usize>> {
    harness(
        |ui, chosen: &mut Option<usize>| {
            let buttons = [
                AlertButton::cancel("Cancel"),
                AlertButton::destructive("Remove"),
            ];
            if let Some(index) = alert(
                ui.ctx(),
                "remove",
                "Remove \"Notes\"?",
                "Files are not deleted.",
                &buttons,
                |_| {},
            ) {
                *chosen = Some(index);
            }
        },
        None,
    )
}

#[test]
fn alert_enter_picks_the_default_cancel_button() {
    let mut harness = remove_alert();
    harness.run();

    harness.key_press(Key::Enter);
    harness.run();

    assert_eq!(*harness.state(), Some(0));
}

#[test]
fn alert_escape_picks_the_cancel_button() {
    let mut harness = remove_alert();
    harness.run();

    harness.key_press(Key::Escape);
    harness.run();

    assert_eq!(*harness.state(), Some(0));
}

#[test]
fn alert_destructive_button_reports_its_index_on_click() {
    let mut harness = remove_alert();
    harness.run();

    harness.get_by_label("Remove").click();
    harness.run();

    assert_eq!(*harness.state(), Some(1));
}

#[test]
fn destructive_button_click_is_reported() {
    let mut harness = harness(
        |ui, clicked: &mut bool| {
            if destructive_button(ui, "Remove").clicked() {
                *clicked = true;
            }
        },
        false,
    );

    harness.get_by_label("Remove").click();
    harness.run();

    assert!(*harness.state());
}

#[test]
fn icon_button_exposes_its_tooltip_as_the_accessible_name() {
    let mut harness = harness(
        |ui, clicks: &mut u32| {
            if icon_button(ui, Icon::Plus, "Add Sync…").clicked() {
                *clicks += 1;
            }
        },
        0,
    );

    let button = harness.get_by_role_and_label(egui::accesskit::Role::Button, "Add Sync…");
    button.click();
    harness.run();

    assert_eq!(*harness.state(), 1);
}

#[test]
fn switch_flips_when_its_label_is_clicked() {
    let mut harness = harness(
        |ui, on: &mut bool| {
            switch(ui, on, "System junk files");
        },
        false,
    );
    assert_eq!(
        harness
            .get_by_label("System junk files")
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::False)
    );

    harness.get_by_label("System junk files").click();
    harness.run();

    assert!(*harness.state());
    assert_eq!(
        harness
            .get_by_label("System junk files")
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::True)
    );
}

#[test]
fn switch_row_spans_the_width_at_control_height() {
    let mut harness = harness(
        |ui, on: &mut bool| {
            switch(ui, on, "Developer folders");
        },
        true,
    );
    harness.run();

    let row = harness.get_by_label("Developer folders").rect();

    assert!((row.height() - theme::size::CONTROL_HEIGHT).abs() < 0.5);
    assert!(row.width() > 300.0, "the row spans the width: {row:?}");
}

#[test]
fn determinate_progress_exposes_its_percentage() {
    let mut harness = harness(
        |ui, ()| {
            progress_bar(ui, "Sync progress", Some(0.42));
        },
        (),
    );
    harness.run();

    let bar = harness.get_by_role(egui::accesskit::Role::ProgressIndicator);

    let value = bar.accesskit_node().numeric_value().unwrap_or_default();
    assert!((value - 42.0).abs() < 0.01, "{value}");
}

#[test]
fn indeterminate_progress_has_no_value() {
    let harness = harness(
        |ui, ()| {
            progress_bar(ui, "Checking", None);
        },
        (),
    );

    let bar = harness.get_by_role(egui::accesskit::Role::ProgressIndicator);

    assert_eq!(bar.accesskit_node().numeric_value(), None);
}

#[test]
fn status_badge_shows_text_except_when_compact() {
    let mut harness = harness(
        |ui, ()| {
            status_badge(ui, DisplayStatus::NeedsReview, BadgeSize::Regular);
            status_badge(ui, DisplayStatus::Conflicts(2), BadgeSize::Compact);
        },
        (),
    );
    harness.run();

    assert!(harness.query_by_label("Review needed").is_some());
    assert!(harness.query_by_label("2 conflicts").is_some());
}

#[test]
fn sidebar_row_announces_title_status_and_subtitle() {
    let mut harness = harness(
        |ui, clicked: &mut bool| {
            let row = SidebarRow {
                icon: Icon::Folder,
                title: "Notes",
                subtitle: Some("Synced 3 min ago"),
                selected: true,
                status: Some(DisplayStatus::UpToDate.into()),
            };
            if sidebar_row(ui, &row).clicked() {
                *clicked = true;
            }
        },
        false,
    );

    harness
        .get_by_label("Notes, Up to date, Synced 3 min ago")
        .click();
    harness.run();

    assert!(*harness.state());
}

#[test]
fn segmented_reports_only_a_change_of_selection() {
    let mut harness = harness(
        |ui, picked: &mut Option<usize>| {
            if let Some(index) = segmented(ui, 0, &["Activity", "Conflicts", "Filters"]) {
                *picked = Some(index);
            }
        },
        None,
    );

    harness.get_by_label("Activity").click();
    harness.run();
    assert_eq!(*harness.state(), None);

    harness.get_by_label("Filters").click();
    harness.run();
    assert_eq!(*harness.state(), Some(2));
}

#[test]
fn inline_error_shows_its_message() {
    let mut harness = harness(
        |ui, ()| {
            inline_error(ui, "remote folder overlaps \"Google Drive\"");
        },
        (),
    );
    harness.run();

    assert!(
        harness
            .query_by_label("remote folder overlaps \"Google Drive\"")
            .is_some()
    );
}

#[test]
fn text_field_is_named_by_its_label_and_takes_typing() {
    let mut harness = harness(
        |ui, value: &mut String| {
            text_field(ui, "Account name", value, "work");
        },
        String::new(),
    );

    let field = harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Account name");
    field.click();
    field.type_text("trabalho");
    harness.run();

    assert_eq!(harness.state(), "trabalho");
}

fn drive_tree() -> Vec<FolderNode> {
    let nested = FolderNode {
        path: "Pessoal/Financeiro".into(),
        name: "Financeiro".into(),
        children: NodeState::Loaded(Vec::new()),
    };
    vec![
        FolderNode {
            path: "Faculdade".into(),
            name: "Faculdade".into(),
            children: NodeState::Unloaded,
        },
        FolderNode {
            path: "Pessoal".into(),
            name: "Pessoal".into(),
            children: NodeState::Loaded(vec![nested]),
        },
    ]
}

fn tree_harness<'a>(expanded: HashSet<String>) -> Harness<'a, Vec<TreeAction>> {
    harness(
        move |ui, actions: &mut Vec<TreeAction>| {
            let nodes = drive_tree();
            let tree = FolderTree {
                root_label: "Entire gdrive",
                nodes: &nodes,
                selected: None,
                expanded: &expanded,
            };
            actions.extend(folder_tree(ui, &tree));
        },
        Vec::new(),
    )
}

#[test]
fn folder_tree_chevron_asks_the_caller_to_load_children() {
    let mut harness = tree_harness(HashSet::new());

    harness.get_by_label("Expand Faculdade").click();
    harness.run();

    assert_eq!(*harness.state(), [TreeAction::Toggle("Faculdade".into())]);
}

#[test]
fn folder_tree_row_click_selects_the_path() {
    let mut harness = tree_harness(HashSet::new());

    harness.get_by_label("Pessoal").click();
    harness.run();

    assert_eq!(*harness.state(), [TreeAction::Select("Pessoal".into())]);
}

#[test]
fn folder_tree_root_row_selects_the_empty_path() {
    let mut harness = tree_harness(HashSet::new());

    harness.get_by_label("Entire gdrive").click();
    harness.run();

    assert_eq!(*harness.state(), [TreeAction::Select(String::new())]);
}

#[test]
fn folder_tree_shows_loaded_children_only_when_expanded() {
    let collapsed = tree_harness(HashSet::new());
    let expanded = tree_harness(HashSet::from(["Pessoal".to_owned()]));

    assert!(collapsed.query_by_label("Financeiro").is_none());
    assert!(expanded.query_by_label("Financeiro").is_some());
    assert!(expanded.query_by_label("Collapse Pessoal").is_some());
}

#[test]
fn themes_list_a_valid_user_theme_and_report_an_invalid_one() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("noctalia.json"),
        r##"{"base":"light","colors":{"accent":"#102030"}}"##,
    )
    .unwrap();
    std::fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
    std::fs::write(
        dir.path().join("typo.json"),
        r##"{"colors":{"acent":"#102030"}}"##,
    )
    .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

    let themes = Themes::load(dir.path());

    assert_eq!(themes.names(), ["noctalia.json"]);
    let errors = themes.errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors
            .iter()
            .any(|error| error.starts_with("broken.json: "))
    );
    assert!(errors.contains(&"typo.json: unknown color: acent".to_owned()));
}

#[test]
fn a_named_theme_resolves_to_its_palette_and_a_missing_one_to_the_system_base() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("noctalia.json"),
        r##"{"base":"light","colors":{"accent":"#102030"}}"##,
    )
    .unwrap();
    let themes = Themes::load(dir.path());

    let named = themes.resolve(&ThemeChoice::Named("noctalia.json".into()), true);
    let gone = themes.resolve(&ThemeChoice::Named("gone.json".into()), true);

    assert!(!named.dark);
    assert_eq!(named.accent, egui::Color32::from_rgb(0x10, 0x20, 0x30));
    assert_eq!(*gone, Palette::dark());
}

#[test]
fn system_choice_follows_the_system_appearance() {
    let themes = Themes::load(std::path::Path::new("/nonexistent/syncfast/themes"));

    assert_eq!(*themes.resolve(&ThemeChoice::System, true), Palette::dark());
    assert_eq!(
        *themes.resolve(&ThemeChoice::System, false),
        Palette::light()
    );
    assert_eq!(*themes.resolve(&ThemeChoice::Light, true), Palette::light());
    assert!(themes.errors().is_empty());
}

#[test]
fn reload_picks_up_a_theme_written_after_the_first_scan() {
    let dir = tempfile::tempdir().unwrap();
    let mut themes = Themes::load(dir.path());
    assert!(themes.names().is_empty());

    std::fs::write(dir.path().join("late.json"), r#"{"base":"dark"}"#).unwrap();
    themes.reload();

    assert_eq!(themes.names(), ["late.json"]);
}

#[test]
fn a_setting_row_puts_the_label_and_caption_left_of_the_control_and_centers_them() {
    let mut harness = harness(
        |ui, (): &mut ()| {
            setting_row(ui, "Theme", Some("Follows the desktop"), |ui| {
                ui.button("Choose")
            });
        },
        (),
    );
    harness.run();

    let label = harness.get_by_label("Theme").rect();
    let caption = harness.get_by_label("Follows the desktop").rect();
    let control = harness.get_by_label("Choose").rect();

    assert!(label.right() <= control.left());
    assert!(caption.right() <= control.left());
    assert!(label.bottom() <= caption.top());
    let text = label.union(caption);
    assert!((text.center().y - control.center().y).abs() < 2.0);
}

#[test]
fn a_setting_row_grows_when_its_caption_wraps_instead_of_spilling_into_the_next_row() {
    let mut harness = harness(
        |ui, (): &mut ()| {
            ui.set_max_width(320.0);
            setting_row(
                ui,
                "Theme",
                Some("A long caption that has to wrap onto several lines in a narrow column"),
                |ui| ui.button("Choose"),
            );
            setting_row(ui, "Next", None, |ui| ui.button("Other"));
        },
        (),
    );
    harness.run();

    let caption = harness.get_by_label_contains("A long caption").rect();
    let control = harness.get_by_label("Choose").rect();
    let next = harness.get_by_label("Next").rect();

    assert!(caption.height() > 2.0 * 13.0, "the caption did not wrap");
    assert!(caption.right() <= control.left());
    assert!(caption.bottom() <= next.top());
}
