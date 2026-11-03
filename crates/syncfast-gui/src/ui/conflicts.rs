//! The Conflicts tab: files changed on both sides at once, where rclone kept
//! the newer one and left the other as `<name>.conflict<N>`.

use egui::{Align, Id, Layout, RichText, Ui};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use syncfast_core::conflicts::{Conflict, FileFacts, Resolution};

use super::widgets::{empty_state, list_row, secondary_button};
use super::{Action, ViewModel};
use crate::icons::Icon;
use crate::theme::{self, space};

const ROW_HEIGHT: f32 = 64.0;

pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let Some(pair) = vm.selected_pair() else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    if just_opened(ui.ctx(), &pair.id) {
        actions.push(Action::RefreshConflicts(pair.id.clone()));
    }
    if vm.conflicts.is_empty() {
        empty_state(ui, Icon::Copy, "No conflicts", None, None);
        return actions;
    }
    let tz = TimeZone::system();
    for conflict in &vm.conflicts {
        ui.push_id(&conflict.path, |ui| {
            actions.extend(row(ui, &pair.id, conflict, vm.now, &tz));
        });
    }
    actions
}

/// True on the first frame the tab is drawn for `pair_id`, and again when
/// frames stopped for a while (another tab, another sync): the folder is
/// looked at again when the user comes back to the list.
fn just_opened(ctx: &egui::Context, pair_id: &str) -> bool {
    let frame = ctx.cumulative_frame_nr();
    let key = Id::new(("conflicts-tab-frame", pair_id));
    let previous = ctx.data_mut(|data| {
        let previous = data.get_temp::<u64>(key);
        data.insert_temp(key, frame);
        previous
    });
    previous.is_none_or(|previous| previous.saturating_add(1) < frame)
}

fn row(
    ui: &mut Ui,
    pair_id: &str,
    conflict: &Conflict,
    now: Timestamp,
    tz: &TimeZone,
) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    list_row(ui, ROW_HEIGHT, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            for (label, resolution) in [
                ("Keep Both", Resolution::KeepBoth),
                ("Keep Conflicted Copy", Resolution::KeepConflicted),
                ("Keep Current", Resolution::KeepCurrent),
            ] {
                if secondary_button(ui, label).clicked() {
                    actions.push(Action::ResolveConflict {
                        id: pair_id.to_owned(),
                        path: conflict.path.clone(),
                        resolution,
                    });
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(
                        egui::Label::new(
                            RichText::new(conflict.original.display().to_string())
                                .font(theme::headline())
                                .color(palette.text),
                        )
                        .truncate(),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(describe(conflict, now, tz))
                                .font(theme::subheadline())
                                .color(palette.secondary),
                        )
                        .truncate(),
                    );
                });
            });
        });
    });
    actions
}

/// `Current: 4.1 KB, today 09:12 · Conflicted copy: 3.8 KB, today 09:10`.
fn describe(conflict: &Conflict, now: Timestamp, tz: &TimeZone) -> String {
    let current = conflict
        .current
        .as_ref()
        .map_or_else(|| "missing".to_owned(), |facts| facts_text(facts, now, tz));
    format!(
        "Current: {current} · Conflicted copy: {}",
        facts_text(&conflict.conflicted, now, tz)
    )
}

fn facts_text(facts: &FileFacts, now: Timestamp, tz: &TimeZone) -> String {
    let size = size_text(facts.size);
    match facts.modified {
        Some(modified) => format!("{size}, {}", when(now, modified, tz)),
        None => size,
    }
}

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// `812 B`, `4.1 KB`, `12 KB`: binary steps, one decimal under ten.
fn size_text(bytes: u64) -> String {
    let (mut unit, mut divisor) = (0, 1_u64);
    while unit + 1 < UNITS.len() && bytes / divisor >= 1024 {
        divisor *= 1024;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let tenths = bytes.saturating_mul(10) / divisor;
    if tenths < 100 {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
    } else {
        format!("{} {}", tenths / 10, UNITS[unit])
    }
}

/// `today 09:12`, `yesterday 18:40`, `Nov 01 09:12`, and with the year for
/// another year.
fn when(now: Timestamp, then: Timestamp, tz: &TimeZone) -> String {
    let (now, then): (Zoned, Zoned) = (now.to_zoned(tz.clone()), then.to_zoned(tz.clone()));
    let (today, day) = (now.date(), then.date());
    let clock = then.strftime("%H:%M");
    if day == today {
        format!("today {clock}")
    } else if today.yesterday().is_ok_and(|yesterday| yesterday == day) {
        format!("yesterday {clock}")
    } else if day.year() == today.year() {
        then.strftime("%b %d %H:%M").to_string()
    } else {
        then.strftime("%b %d %Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn sizes_use_binary_steps_with_one_decimal_under_ten() {
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(1024), "1.0 KB");
        assert_eq!(size_text(4200), "4.1 KB");
        assert_eq!(size_text(12 * 1024), "12 KB");
        assert_eq!(size_text(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
        assert_eq!(size_text(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn times_say_today_and_yesterday_then_the_date() {
        let now = at("2026-11-03T12:00:00Z");
        let utc = TimeZone::UTC;

        assert_eq!(when(now, at("2026-11-03T09:12:00Z"), &utc), "today 09:12");
        assert_eq!(
            when(now, at("2026-11-02T18:40:00Z"), &utc),
            "yesterday 18:40"
        );
        assert_eq!(when(now, at("2026-10-30T07:05:00Z"), &utc), "Oct 30 07:05");
        assert_eq!(when(now, at("2025-12-31T07:05:00Z"), &utc), "Dec 31 2025");
    }

    #[test]
    fn a_missing_current_file_is_said_in_the_line() {
        let conflict = Conflict {
            path: "a.md.conflict1".into(),
            original: "a.md".into(),
            current: None,
            conflicted: FileFacts {
                size: 3900,
                modified: None,
            },
        };

        let text = describe(&conflict, at("2026-11-03T12:00:00Z"), &TimeZone::UTC);

        assert_eq!(text, "Current: missing · Conflicted copy: 3.8 KB");
    }
}
