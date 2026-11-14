//! The Recently Deleted tab: what a sync deleted or replaced that can still
//! come back, from its versions folders or the Drive trash, with a button
//! that copies each one back into the sync's folder.

use std::fmt::Write as _;

use egui::{Align, Layout, RichText, Ui};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use syncfast_core::recently_deleted::{DeletedItem, DeletedKind, DeletedSide};

use super::conflicts::when;
use super::detail::just_opened;
use super::relative::ago;
use super::widgets::{empty_state, inline_error, list_row, secondary_button, warning_row};
use super::{Action, DetailTab, ViewModel};
use crate::icons::Icon;
use crate::theme::{self, space};

const ROW_HEIGHT: f32 = 64.0;
const LINE_GAP: f32 = 2.0;
const NOT_KEPT: &str = "Files deleted on this computer are not kept. Change this in Options.";

pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let Some(pair) = vm.selected_pair() else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    if just_opened(ui.ctx(), DetailTab::RecentlyDeleted, &pair.id) {
        actions.push(Action::RefreshDeleted(pair.id.clone()));
    }
    let list = match &vm.deleted {
        None => {
            empty_state(ui, Icon::Clock, "Checking…", None, None);
            return actions;
        }
        Some(Err(message)) => {
            inline_error(ui, message);
            return actions;
        }
        Some(Ok(list)) => list,
    };
    if let Some(warning) = &list.warning {
        warning_row(ui, warning);
        ui.add_space(space::S);
    }
    if list.items.is_empty() {
        let note = (!pair.effective.behavior.versions).then_some(NOT_KEPT);
        empty_state(ui, Icon::Trash, "Nothing deleted recently", note, None);
        return actions;
    }
    let tz = TimeZone::system();
    for item in &list.items {
        ui.push_id((&item.key, item.side), |ui| {
            if row(ui, item, vm.now, &tz) {
                actions.push(Action::RestoreDeleted {
                    id: pair.id.clone(),
                    key: item.key.clone(),
                    side: item.side,
                });
            }
        });
    }
    actions
}

/// One file and its `Restore` button; true when the button was clicked.
fn row(ui: &mut Ui, item: &DeletedItem, now: Timestamp, tz: &TimeZone) -> bool {
    let palette = theme::palette(ui.ctx());
    let mut clicked = false;
    list_row(ui, ROW_HEIGHT, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            clicked = secondary_button(ui, "Restore").clicked();
            let (title, detail) = (theme::headline(), theme::subheadline());
            let text_height = ui
                .ctx()
                .fonts_mut(|fonts| fonts.row_height(&title) + LINE_GAP + fonts.row_height(&detail));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = LINE_GAP;
                ui.add_space(((ui.available_height() - text_height) / 2.0).max(0.0));
                ui.add(
                    egui::Label::new(RichText::new(&item.path).font(title).color(palette.text))
                        .truncate(),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(describe(item, now, tz))
                            .font(detail)
                            .color(palette.secondary),
                    )
                    .truncate(),
                );
            });
        });
    });
    clicked
}

/// `Deleted on this computer 2 h ago · Kept until Nov 30 14:05`.
fn describe(item: &DeletedItem, now: Timestamp, tz: &TimeZone) -> String {
    let caption = match (item.side, item.kind) {
        (DeletedSide::Drive, _) => return "In the Drive trash".to_owned(),
        (DeletedSide::Local, DeletedKind::Deleted) => "Deleted on this computer",
        (DeletedSide::Local, DeletedKind::Replaced) => "Replaced on this computer",
        (DeletedSide::Remote, DeletedKind::Deleted) => "Deleted on Drive",
        (DeletedSide::Remote, DeletedKind::Replaced) => "Replaced on Drive",
    };
    let mut text = match item.when {
        Some(at) => format!("{caption} {}", ago(now, at)),
        None => caption.to_owned(),
    };
    if let Some(until) = item.kept_until {
        let _ = write!(text, " · Kept until {}", when(now, until, tz));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn item(side: DeletedSide, kind: DeletedKind, when: Option<&str>) -> DeletedItem {
        let when = when.map(at);
        DeletedItem {
            side,
            kind,
            path: "a.txt".to_owned(),
            when,
            kept_until: when.map(|when| when + jiff::SignedDuration::from_hours(30 * 24)),
            size: 1,
            key: "k".to_owned(),
        }
    }

    #[test]
    fn a_line_says_where_the_file_went_how_long_ago_and_until_when_it_is_kept() {
        let now = at("2026-11-03T12:00:00Z");
        let utc = TimeZone::UTC;

        assert_eq!(
            describe(
                &item(
                    DeletedSide::Local,
                    DeletedKind::Deleted,
                    Some("2026-11-03T10:00:00Z")
                ),
                now,
                &utc
            ),
            "Deleted on this computer 2 h ago · Kept until Dec 03 10:00"
        );
        assert_eq!(
            describe(
                &item(
                    DeletedSide::Remote,
                    DeletedKind::Replaced,
                    Some("2026-11-03T11:59:30Z")
                ),
                now,
                &utc
            ),
            "Replaced on Drive just now · Kept until Dec 03 11:59"
        );
        assert_eq!(
            describe(
                &item(DeletedSide::Drive, DeletedKind::Deleted, None),
                now,
                &utc
            ),
            "In the Drive trash"
        );
    }
}
