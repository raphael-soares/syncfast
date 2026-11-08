//! The tray item: a status line, a fixed menu, and a glyph that tells the
//! worst state among the syncs. The pure parts (what the menu and the glyph
//! say) are separate from the item itself so they can be tested.

use std::sync::{Mutex, PoisonError};

use fastframe_shell::Waker;
use fastframe_tray::{Config, DrawIcon, Event, IconColour, MenuItem, Tray};
use resvg::tiny_skia::{BlendMode, FillRule, Paint, Path, PathBuilder, Pixmap, Transform};
use resvg::usvg;
use syncfast_core::backup::status::BackupStatus;
use syncfast_core::state::DisplayStatus;

use crate::icons::{Icon, svg};
use crate::ui::Action;

pub const ID_STATUS: &str = "status";
pub const ID_TOGGLE: &str = "toggle";
pub const ID_SYNC_ALL: &str = "sync-all";
pub const ID_BACKUPS: &str = "backups";
pub const ID_BACKUP_ALL: &str = "backup-all";
pub const ID_PAUSE: &str = "pause";
pub const ID_SETTINGS: &str = "settings";
pub const ID_QUIT: &str = "quit";

/// The one place besides the theme that holds literal colours: they must
/// read on any panel, so they do not follow the palette.
const ATTENTION_DOT: [u8; 3] = [0xF2, 0xB8, 0x5C];
const ERROR_DOT: [u8; 3] = [0xF5, 0x71, 0x7F];
const INK_ON_DARK: &str = "#ffffff";
const INK_ON_LIGHT: &str = "#000000";
const DOT_SIZE: f32 = 0.35;
/// The clear ring around the dot, as a multiple of the dot's radius.
const DOT_HALO: f32 = 1.35;
const GLYPH_VIEWBOX: f32 = 24.0;
/// The icon files are stroked in white so egui can tint them.
const GLYPH_SOURCE_INK: &str = "#ffffff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairView {
    pub status: DisplayStatus,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupView {
    pub status: BackupStatus,
    pub runnable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    pub syncing: usize,
    pub attention: usize,
    pub offline: usize,
    pub paused: usize,
    pub any_enabled: bool,
    pub worst: Option<DisplayStatus>,
}

/// How bad a status is for the glyph, the lower the worse: what asks the
/// user to act comes before a round in progress, a pause or no network. It
/// is not the order of `display_status`, which ranks one sync, not the
/// worst of several.
fn rank(status: DisplayStatus) -> u8 {
    match status {
        DisplayStatus::Error => 0,
        DisplayStatus::NeedsReview => 1,
        DisplayStatus::Conflicts(_) => 2,
        DisplayStatus::Failing => 3,
        DisplayStatus::Syncing => 4,
        DisplayStatus::Paused => 5,
        DisplayStatus::Offline => 6,
        DisplayStatus::Retrying => 7,
        DisplayStatus::UpToDate => 8,
    }
}

/// Statuses that ask the user to do something.
fn needs_attention(status: DisplayStatus) -> bool {
    matches!(
        status,
        DisplayStatus::Error
            | DisplayStatus::NeedsReview
            | DisplayStatus::Conflicts(_)
            | DisplayStatus::Failing
    )
}

impl Summary {
    #[must_use]
    pub fn of(pairs: &[PairView]) -> Self {
        let count_of = |matches: fn(DisplayStatus) -> bool| {
            pairs.iter().filter(|pair| matches(pair.status)).count()
        };
        Self {
            count: pairs.len(),
            syncing: count_of(|status| status == DisplayStatus::Syncing),
            attention: count_of(needs_attention),
            offline: count_of(|status| status == DisplayStatus::Offline),
            paused: count_of(|status| status == DisplayStatus::Paused),
            any_enabled: pairs.iter().any(|pair| pair.enabled),
            worst: pairs
                .iter()
                .map(|pair| pair.status)
                .min_by_key(|s| rank(*s)),
        }
    }

    /// The disabled first line of the menu.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.count == 0 {
            "No syncs yet".to_owned()
        } else if self.syncing > 0 {
            format!("Syncing {}…", self.syncing)
        } else if self.attention > 0 {
            format!("{} syncs need attention", self.attention)
        } else if self.offline > 0 {
            "Offline".to_owned()
        } else if self.paused == self.count {
            "All syncs paused".to_owned()
        } else {
            "All syncs up to date".to_owned()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSummary {
    pub count: usize,
    pub runnable: usize,
    pub running: usize,
    pub attention: usize,
    pub offline: usize,
    pub unavailable: usize,
    pub paused: usize,
}

impl BackupSummary {
    #[must_use]
    pub fn of(backups: &[BackupView]) -> Self {
        let count_of = |matches: fn(BackupStatus) -> bool| {
            backups
                .iter()
                .filter(|backup| matches(backup.status))
                .count()
        };
        Self {
            count: backups.len(),
            runnable: backups.iter().filter(|backup| backup.runnable).count(),
            running: count_of(|status| {
                matches!(status, BackupStatus::BackingUp | BackupStatus::Checking)
            }),
            attention: count_of(|status| {
                matches!(status, BackupStatus::Error | BackupStatus::Failing)
            }),
            offline: count_of(|status| status == BackupStatus::Offline),
            unavailable: count_of(|status| status == BackupStatus::Unavailable),
            paused: count_of(|status| status == BackupStatus::Paused),
        }
    }

    /// The disabled line of the menu below the one of the syncs.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.count == 0 {
            "No backups yet".to_owned()
        } else if self.running > 0 {
            "Backing up…".to_owned()
        } else if self.attention > 0 {
            format!("{} backups need attention", self.attention)
        } else if self.offline > 0 {
            "Offline".to_owned()
        } else if self.unavailable > 0 {
            "Destination not available".to_owned()
        } else if self.paused == self.count {
            "All backups paused".to_owned()
        } else {
            "Backups up to date".to_owned()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    FolderSync,
    RefreshCw,
    CloudOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    None,
    Attention,
    Error,
}

/// Whether the panel the item sits on is dark, which decides the colour of
/// the glyph; the dot keeps its own colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    White,
    Black,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconLook {
    pub glyph: Glyph,
    pub dot: Dot,
    pub ink: Ink,
}

impl IconLook {
    /// `panel_dark` is `None` when the desktop does not say; most panels
    /// are dark.
    #[must_use]
    pub fn of(worst: Option<DisplayStatus>, panel_dark: Option<bool>) -> Self {
        let glyph = match worst {
            Some(DisplayStatus::Syncing) => Glyph::RefreshCw,
            Some(DisplayStatus::Offline) => Glyph::CloudOff,
            _ => Glyph::FolderSync,
        };
        let dot = match worst {
            Some(DisplayStatus::NeedsReview | DisplayStatus::Conflicts(_)) => Dot::Attention,
            Some(DisplayStatus::Error | DisplayStatus::Failing) => Dot::Error,
            _ => Dot::None,
        };
        let ink = if panel_dark == Some(false) {
            Ink::Black
        } else {
            Ink::White
        };
        Self { glyph, dot, ink }
    }

    /// A backup that needs attention or fails puts the red dot on the icon,
    /// whatever the syncs show.
    #[must_use]
    pub fn counting(self, backups: &BackupSummary) -> Self {
        if backups.attention > 0 {
            Self {
                dot: Dot::Error,
                ..self
            }
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    pub summary: Summary,
    pub backups: BackupSummary,
    pub window_hidden: bool,
    pub look: IconLook,
}

impl TrayView {
    /// `Pause All` while any sync is active, `Resume All` once none is.
    #[must_use]
    pub fn pauses(&self) -> bool {
        self.summary.any_enabled
    }

    fn toggle_label(&self) -> &'static str {
        if self.window_hidden {
            "Show syncfast"
        } else {
            "Hide syncfast"
        }
    }

    fn pause_label(&self) -> &'static str {
        if self.pauses() {
            "Pause All"
        } else {
            "Resume All"
        }
    }

    fn has_syncs(&self) -> bool {
        self.summary.count > 0
    }
}

/// The menu. The same items always, disabled instead of hidden.
#[must_use]
pub fn menu_items(view: &TrayView) -> Vec<MenuItem> {
    vec![
        MenuItem::action(ID_STATUS, view.summary.status_line()).enabled(false),
        MenuItem::action(ID_BACKUPS, view.backups.status_line()).enabled(false),
        MenuItem::Separator,
        MenuItem::action(ID_TOGGLE, view.toggle_label()),
        MenuItem::action(ID_SYNC_ALL, "Sync All Now").enabled(view.has_syncs()),
        MenuItem::action(ID_BACKUP_ALL, "Back Up All Now").enabled(view.backups.runnable > 0),
        MenuItem::action(ID_PAUSE, view.pause_label()).enabled(view.has_syncs()),
        MenuItem::Separator,
        MenuItem::action(ID_SETTINGS, "Settings…"),
        MenuItem::action(ID_QUIT, "Quit syncfast"),
    ]
}

#[must_use]
pub fn action(event: &Event, view: &TrayView) -> Option<Action> {
    let toggle = || {
        if view.window_hidden {
            Action::ShowWindow
        } else {
            Action::HideWindow
        }
    };
    match event {
        Event::Show => Some(Action::ShowWindow),
        Event::Toggle | Event::Menu(ID_TOGGLE) => Some(toggle()),
        Event::Menu(ID_SYNC_ALL) => Some(Action::SyncAll),
        Event::Menu(ID_BACKUP_ALL) => Some(Action::BackUpAll),
        Event::Menu(ID_PAUSE) => Some(Action::SetAllEnabled(!view.pauses())),
        Event::Menu(ID_SETTINGS) => Some(Action::OpenSettings),
        Event::Menu(ID_QUIT) => Some(Action::Quit),
        Event::Menu(_) => None,
    }
}

fn glyph_svg(glyph: Glyph) -> &'static str {
    svg(match glyph {
        Glyph::FolderSync => Icon::FolderSync,
        Glyph::RefreshCw => Icon::Refresh,
        Glyph::CloudOff => Icon::CloudOff,
    })
}

fn circle(cx: f32, cy: f32, radius: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, radius)
}

fn paint(rgb: [u8; 3], blend: BlendMode) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgb[0], rgb[1], rgb[2], 255);
    paint.blend_mode = blend;
    paint.anti_alias = true;
    paint
}

/// The glyph as straight-alpha RGBA, `size` pixels square, with the dot of
/// `look` in the bottom right corner. A blank icon if the glyph cannot be
/// drawn.
#[must_use]
pub fn render(look: IconLook, size: usize) -> Vec<u8> {
    draw_pixmap(look, size).map_or_else(
        || vec![0; size * size * 4],
        |pixmap| {
            pixmap
                .pixels()
                .iter()
                .flat_map(|pixel| {
                    let color = pixel.demultiply();
                    [color.red(), color.green(), color.blue(), color.alpha()]
                })
                .collect()
        },
    )
}

fn draw_pixmap(look: IconLook, size: usize) -> Option<Pixmap> {
    let edge = u16::try_from(size.max(1)).ok()?;
    let ink = match look.ink {
        Ink::White => INK_ON_DARK,
        Ink::Black => INK_ON_LIGHT,
    };
    let source = glyph_svg(look.glyph).replace(GLYPH_SOURCE_INK, ink);
    let tree = usvg::Tree::from_data(source.as_bytes(), &usvg::Options::default()).ok()?;
    let mut pixmap = Pixmap::new(u32::from(edge), u32::from(edge))?;
    let scale = f32::from(edge) / GLYPH_VIEWBOX;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let dot = match look.dot {
        Dot::None => return Some(pixmap),
        Dot::Attention => ATTENTION_DOT,
        Dot::Error => ERROR_DOT,
    };
    let radius = f32::from(edge) * DOT_SIZE / 2.0;
    let center = f32::from(edge) - radius;
    // A clear ring keeps the dot readable over the glyph's strokes.
    if let Some(halo) = circle(center, center, radius * DOT_HALO) {
        pixmap.fill_path(
            &halo,
            &paint([0, 0, 0], BlendMode::Clear),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let disc = circle(center, center, radius)?;
    pixmap.fill_path(
        &disc,
        &paint(dot, BlendMode::SourceOver),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    Some(pixmap)
}

/// `DrawIcon` is a bare function pointer, so what it draws is read from here.
static CURRENT_LOOK: Mutex<IconLook> = Mutex::new(IconLook {
    glyph: Glyph::FolderSync,
    dot: Dot::None,
    ink: Ink::White,
});

fn draw_current(size: usize) -> Vec<u8> {
    let look = *CURRENT_LOOK.lock().unwrap_or_else(PoisonError::into_inner);
    render(look, size)
}

const DRAW: DrawIcon = draw_current;

#[derive(Debug)]
pub struct SyncTray {
    tray: Tray,
    view: TrayView,
}

impl SyncTray {
    /// `None` when the session has no bus for the item.
    #[must_use]
    pub fn spawn(waker: &Waker, view: TrayView) -> Option<Self> {
        *CURRENT_LOOK.lock().unwrap_or_else(PoisonError::into_inner) = view.look;
        let wake = waker.clone();
        let tray = Tray::spawn(
            Config {
                id: "syncfast",
                title: "syncfast".into(),
                icon: DRAW,
                template_icon: None,
                // The glyph changes with the state, so the host must not
                // draw the installed app icon by name instead.
                themed_icon: false,
                menu: menu_items(&view),
            },
            move || wake.wake(),
        )?;
        let mut tray = Self { tray, view };
        tray.tray.set_icon_colour(IconColour::Original);
        Some(tray)
    }

    /// Updates only what changed since the last call.
    pub fn sync(&mut self, view: &TrayView) {
        if *view == self.view {
            return;
        }
        let items = menu_items(view);
        let before = menu_items(&self.view);
        for (item, old) in items.iter().zip(&before) {
            if item == old {
                continue;
            }
            if let MenuItem::Action {
                id, label, enabled, ..
            } = item
            {
                self.tray.set_label(id, label.clone());
                self.tray.set_enabled(id, *enabled);
            }
        }
        if view.look != self.view.look {
            *CURRENT_LOOK.lock().unwrap_or_else(PoisonError::into_inner) = view.look;
            self.tray.set_icon(DRAW, None);
        }
        self.tray
            .set_tooltip(format!("syncfast\n{}", view.summary.status_line()));
        self.view = view.clone();
    }

    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.tray.events()
    }

    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.tray.is_shown()
    }

    pub fn attach(&mut self) {
        self.tray.attach();
    }

    pub fn create_item(&mut self) {
        self.tray.create_item();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(status: DisplayStatus, enabled: bool) -> PairView {
        PairView { status, enabled }
    }

    fn view(pairs: &[PairView], window_hidden: bool) -> TrayView {
        let summary = Summary::of(pairs);
        TrayView {
            look: IconLook::of(summary.worst, Some(true)),
            summary,
            backups: BackupSummary::of(&[]),
            window_hidden,
        }
    }

    fn backup(status: BackupStatus, runnable: bool) -> BackupView {
        BackupView { status, runnable }
    }

    fn running_backups(statuses: &[BackupStatus]) -> Vec<BackupView> {
        statuses
            .iter()
            .map(|status| backup(*status, true))
            .collect()
    }

    fn with_backup_views(mut view: TrayView, backups: &[BackupView]) -> TrayView {
        view.backups = BackupSummary::of(backups);
        view.look = view.look.counting(&view.backups);
        view
    }

    fn with_backups(view: TrayView, statuses: &[BackupStatus]) -> TrayView {
        with_backup_views(view, &running_backups(statuses))
    }

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                MenuItem::Action { label, .. } => label.clone(),
                MenuItem::Separator => "---".to_owned(),
            })
            .collect()
    }

    #[test]
    fn status_line_says_there_are_no_syncs_when_the_list_is_empty() {
        assert_eq!(Summary::of(&[]).status_line(), "No syncs yet");
    }

    #[test]
    fn status_line_counts_the_syncs_that_are_running() {
        let pairs = [
            pair(DisplayStatus::Syncing, true),
            pair(DisplayStatus::Syncing, true),
            pair(DisplayStatus::UpToDate, true),
        ];

        assert_eq!(Summary::of(&pairs).status_line(), "Syncing 2…");
    }

    #[test]
    fn status_line_counts_the_syncs_that_need_attention() {
        let pairs = [
            pair(DisplayStatus::NeedsReview, true),
            pair(DisplayStatus::Conflicts(2), true),
            pair(DisplayStatus::Failing, true),
            pair(DisplayStatus::Error, true),
            pair(DisplayStatus::UpToDate, true),
        ];

        assert_eq!(Summary::of(&pairs).status_line(), "4 syncs need attention");
    }

    #[test]
    fn status_line_says_offline_when_a_sync_is_offline_and_none_needs_attention() {
        let pairs = [
            pair(DisplayStatus::Offline, true),
            pair(DisplayStatus::UpToDate, true),
        ];

        assert_eq!(Summary::of(&pairs).status_line(), "Offline");
    }

    #[test]
    fn status_line_says_all_paused_only_when_every_sync_is_paused() {
        let all = [
            pair(DisplayStatus::Paused, false),
            pair(DisplayStatus::Paused, false),
        ];
        let some = [
            pair(DisplayStatus::Paused, false),
            pair(DisplayStatus::UpToDate, true),
        ];

        assert_eq!(Summary::of(&all).status_line(), "All syncs paused");
        assert_eq!(Summary::of(&some).status_line(), "All syncs up to date");
    }

    #[test]
    fn retrying_syncs_do_not_ask_for_attention() {
        let pairs = [pair(DisplayStatus::Retrying, true)];

        assert_eq!(Summary::of(&pairs).status_line(), "All syncs up to date");
    }

    #[test]
    fn menu_has_the_same_ten_items_with_or_without_syncs() {
        let empty = menu_items(&view(&[], false));
        let busy = menu_items(&view(&[pair(DisplayStatus::UpToDate, true)], false));

        assert_eq!(empty.len(), 10);
        assert_eq!(
            labels(&busy),
            [
                "All syncs up to date",
                "No backups yet",
                "---",
                "Hide syncfast",
                "Sync All Now",
                "Back Up All Now",
                "Pause All",
                "---",
                "Settings…",
                "Quit syncfast"
            ]
        );
        assert_eq!(labels(&empty)[3..6], labels(&busy)[3..6]);
        assert_eq!(labels(&empty)[7..], labels(&busy)[7..]);
    }

    #[test]
    fn backups_line_says_what_the_backups_are_doing() {
        let line =
            |statuses: &[BackupStatus]| BackupSummary::of(&running_backups(statuses)).status_line();

        assert_eq!(line(&[]), "No backups yet");
        assert_eq!(line(&[BackupStatus::UpToDate]), "Backups up to date");
        assert_eq!(
            line(&[BackupStatus::BackingUp, BackupStatus::Error]),
            "Backing up…"
        );
        assert_eq!(
            line(&[
                BackupStatus::Error,
                BackupStatus::Failing,
                BackupStatus::UpToDate
            ]),
            "2 backups need attention"
        );
        assert_eq!(line(&[BackupStatus::Retrying]), "Backups up to date");
        assert_eq!(line(&[BackupStatus::Paused]), "All backups paused");
        assert_eq!(
            line(&[BackupStatus::Paused, BackupStatus::Paused]),
            "All backups paused"
        );
        assert_eq!(
            line(&[BackupStatus::Paused, BackupStatus::UpToDate]),
            "Backups up to date"
        );
        assert_eq!(
            line(&[BackupStatus::Offline, BackupStatus::UpToDate]),
            "Offline"
        );
        assert_eq!(
            line(&[BackupStatus::Unavailable, BackupStatus::Paused]),
            "Destination not available"
        );
    }

    #[test]
    fn back_up_all_is_disabled_without_backups_and_runs_every_backup() {
        let none = view(&[], false);
        let some = with_backups(view(&[], false), &[BackupStatus::UpToDate]);

        assert!(!enabled(&menu_items(&none), ID_BACKUP_ALL));
        assert!(enabled(&menu_items(&some), ID_BACKUP_ALL));
        assert!(!enabled(&menu_items(&some), ID_BACKUPS));
        assert_eq!(
            action(&Event::Menu(ID_BACKUP_ALL), &some),
            Some(Action::BackUpAll)
        );
    }

    #[test]
    fn back_up_all_is_disabled_when_every_backup_is_paused_or_has_no_folder() {
        let stopped = with_backup_views(
            view(&[], false),
            &[
                backup(BackupStatus::Paused, false),
                backup(BackupStatus::Paused, false),
            ],
        );
        let one_runs = with_backup_views(
            view(&[], false),
            &[
                backup(BackupStatus::Paused, false),
                backup(BackupStatus::UpToDate, true),
            ],
        );

        assert!(!enabled(&menu_items(&stopped), ID_BACKUP_ALL));
        assert!(enabled(&menu_items(&one_runs), ID_BACKUP_ALL));
    }

    #[test]
    fn a_backup_that_fails_puts_the_red_dot_on_the_icon_of_calm_syncs() {
        let calm = view(&[pair(DisplayStatus::UpToDate, true)], false);

        assert_eq!(calm.look.dot, Dot::None);
        for status in [BackupStatus::Error, BackupStatus::Failing] {
            let dotted = with_backups(calm.clone(), &[status]);
            assert_eq!(dotted.look.dot, Dot::Error, "{status:?}");
        }
        let retrying = with_backups(calm, &[BackupStatus::Retrying, BackupStatus::Offline]);
        assert_eq!(retrying.look.dot, Dot::None);
    }

    fn enabled(items: &[MenuItem], wanted: &str) -> bool {
        items
            .iter()
            .any(|item| matches!(item, MenuItem::Action { id, enabled: true, .. } if *id == wanted))
    }

    #[test]
    fn sync_all_and_pause_are_disabled_without_syncs_and_enabled_with_them() {
        let empty = menu_items(&view(&[], false));
        let busy = menu_items(&view(&[pair(DisplayStatus::UpToDate, true)], false));

        assert!(!enabled(&empty, ID_SYNC_ALL));
        assert!(!enabled(&empty, ID_PAUSE));
        assert!(enabled(&busy, ID_SYNC_ALL));
        assert!(enabled(&busy, ID_PAUSE));
        assert!(
            !enabled(&busy, ID_STATUS),
            "the status line is never chosen"
        );
        assert!(enabled(&empty, ID_SETTINGS) && enabled(&empty, ID_QUIT));
    }

    #[test]
    fn toggle_item_follows_the_window() {
        let shown = menu_items(&view(&[], false));
        let hidden = menu_items(&view(&[], true));

        assert_eq!(labels(&shown)[3], "Hide syncfast");
        assert_eq!(labels(&hidden)[3], "Show syncfast");
    }

    #[test]
    fn pause_all_becomes_resume_all_once_no_sync_is_active() {
        let active = view(
            &[
                pair(DisplayStatus::Paused, false),
                pair(DisplayStatus::UpToDate, true),
            ],
            false,
        );
        let none = view(&[pair(DisplayStatus::Paused, false)], false);

        assert_eq!(labels(&menu_items(&active))[6], "Pause All");
        assert_eq!(labels(&menu_items(&none))[6], "Resume All");
        assert_eq!(
            action(&Event::Menu(ID_PAUSE), &active),
            Some(Action::SetAllEnabled(false))
        );
        assert_eq!(
            action(&Event::Menu(ID_PAUSE), &none),
            Some(Action::SetAllEnabled(true))
        );
    }

    #[test]
    fn left_click_toggles_the_window_and_the_menu_items_map_to_actions() {
        let shown = view(&[], false);
        let hidden = view(&[], true);

        assert_eq!(action(&Event::Toggle, &shown), Some(Action::HideWindow));
        assert_eq!(action(&Event::Toggle, &hidden), Some(Action::ShowWindow));
        assert_eq!(
            action(&Event::Menu(ID_TOGGLE), &hidden),
            Some(Action::ShowWindow)
        );
        assert_eq!(action(&Event::Show, &shown), Some(Action::ShowWindow));
        assert_eq!(
            action(&Event::Menu(ID_SYNC_ALL), &shown),
            Some(Action::SyncAll)
        );
        assert_eq!(
            action(&Event::Menu(ID_SETTINGS), &shown),
            Some(Action::OpenSettings)
        );
        assert_eq!(action(&Event::Menu(ID_QUIT), &shown), Some(Action::Quit));
        assert_eq!(action(&Event::Menu(ID_STATUS), &shown), None);
    }

    fn worst(statuses: &[DisplayStatus]) -> Option<DisplayStatus> {
        let pairs: Vec<PairView> = statuses.iter().map(|s| pair(*s, true)).collect();
        Summary::of(&pairs).worst
    }

    #[test]
    fn worst_state_puts_what_needs_the_user_above_syncing_paused_and_offline() {
        use DisplayStatus::*;

        assert_eq!(worst(&[Syncing, Failing]), Some(Failing));
        assert_eq!(worst(&[Syncing, Conflicts(1), Failing]), Some(Conflicts(1)));
        assert_eq!(worst(&[Syncing, NeedsReview, Error]), Some(Error));
        assert_eq!(worst(&[UpToDate, NeedsReview, Failing]), Some(NeedsReview));
        assert_eq!(worst(&[Offline, Paused, Syncing]), Some(Syncing));
        assert_eq!(worst(&[Offline, Paused]), Some(Paused));
        assert_eq!(worst(&[UpToDate, Retrying, Offline]), Some(Offline));
        assert_eq!(worst(&[Retrying, UpToDate]), Some(Retrying));
        assert_eq!(worst(&[]), None);
    }

    #[test]
    fn glyph_is_the_folder_unless_syncing_or_offline() {
        let glyph = |status| IconLook::of(Some(status), Some(true)).glyph;

        assert_eq!(glyph(DisplayStatus::UpToDate), Glyph::FolderSync);
        assert_eq!(glyph(DisplayStatus::Error), Glyph::FolderSync);
        assert_eq!(glyph(DisplayStatus::Syncing), Glyph::RefreshCw);
        assert_eq!(glyph(DisplayStatus::Offline), Glyph::CloudOff);
        assert_eq!(IconLook::of(None, None).glyph, Glyph::FolderSync);
    }

    #[test]
    fn dot_is_amber_for_attention_red_for_errors_and_absent_otherwise() {
        let dot = |status| IconLook::of(Some(status), Some(true)).dot;

        assert_eq!(dot(DisplayStatus::NeedsReview), Dot::Attention);
        assert_eq!(dot(DisplayStatus::Conflicts(1)), Dot::Attention);
        assert_eq!(dot(DisplayStatus::Error), Dot::Error);
        assert_eq!(dot(DisplayStatus::Failing), Dot::Error);
        assert_eq!(dot(DisplayStatus::UpToDate), Dot::None);
        assert_eq!(dot(DisplayStatus::Syncing), Dot::None);
        assert_eq!(dot(DisplayStatus::Paused), Dot::None);
    }

    #[test]
    fn ink_is_black_on_a_light_panel_and_white_otherwise() {
        let ink = |dark| IconLook::of(None, dark).ink;

        assert_eq!(ink(Some(false)), Ink::Black);
        assert_eq!(ink(Some(true)), Ink::White);
        assert_eq!(ink(None), Ink::White);
    }

    fn pixel(rgba: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * size + x) * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    }

    const SIZE: usize = 64;

    fn look(glyph: Glyph, dot: Dot, ink: Ink) -> IconLook {
        IconLook { glyph, dot, ink }
    }

    #[test]
    fn glyph_is_drawn_in_the_ink_of_the_panel_and_nothing_else() {
        for (ink, expected) in [(Ink::White, 255), (Ink::Black, 0)] {
            let rgba = render(look(Glyph::FolderSync, Dot::None, ink), SIZE);

            let drawn: Vec<[u8; 4]> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .copied()
                .filter(|px| px[3] > 0)
                .collect();
            assert!(!drawn.is_empty(), "the glyph left pixels");
            // Edge pixels are blended, so allow for rounding in the unpremultiply.
            assert!(
                drawn
                    .iter()
                    .all(|px| px[..3].iter().all(|c| c.abs_diff(expected) <= 4))
            );
        }
    }

    #[test]
    fn dot_sits_in_the_bottom_right_corner_in_its_fixed_colour() {
        let attention = render(look(Glyph::FolderSync, Dot::Attention, Ink::White), SIZE);
        let error = render(look(Glyph::FolderSync, Dot::Error, Ink::Black), SIZE);
        let none = render(look(Glyph::FolderSync, Dot::None, Ink::White), SIZE);
        // The dot's centre: 35% of the icon, tucked into the corner.
        let centre = SIZE - (SIZE * 35 / 100) / 2 - 1;

        assert_eq!(
            pixel(&attention, SIZE, centre, centre),
            [0xF2, 0xB8, 0x5C, 255]
        );
        assert_eq!(pixel(&error, SIZE, centre, centre), [0xF5, 0x71, 0x7F, 255]);
        assert_ne!(pixel(&none, SIZE, centre, centre)[..3], [0xF2, 0xB8, 0x5C]);
    }

    #[test]
    fn each_glyph_draws_something_different() {
        let draw = |glyph| render(look(glyph, Dot::None, Ink::White), SIZE);

        let folder = draw(Glyph::FolderSync);
        let refresh = draw(Glyph::RefreshCw);
        let cloud = draw(Glyph::CloudOff);

        assert_ne!(folder, refresh);
        assert_ne!(folder, cloud);
        assert_ne!(refresh, cloud);
    }
}
