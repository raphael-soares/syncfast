use std::collections::HashSet;

use egui::{Sense, Ui, Vec2};

use crate::icons::Icon;
use crate::theme::{self, radius, size, space};

use super::{focus_ring, paint_icon, palette_of};

const ROW_HEIGHT: f32 = 28.0;
const INDENT: f32 = 16.0;
const CHEVRON_SLOT: f32 = 20.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeState {
    /// Children not requested yet: expanding emits `TreeAction::Toggle` and
    /// the caller loads them.
    Unloaded,
    Loading,
    Loaded(Vec<FolderNode>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderNode {
    /// Identifies the node in actions, e.g. the remote path.
    pub path: String,
    pub name: String,
    pub children: NodeState,
}

#[derive(Debug)]
pub struct FolderTree<'a> {
    /// The always-present first row, e.g. `Entire gdrive`, with path `""`.
    pub root_label: &'a str,
    pub nodes: &'a [FolderNode],
    pub selected: Option<&'a str>,
    pub expanded: &'a HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeAction {
    /// Select the node at this path (`""` is the root row).
    Select(String),
    /// Expand or collapse the node. Load its children when `Unloaded`.
    Toggle(String),
}

/// A folder tree whose state lives with the caller. Children are loaded on
/// demand: the caller answers `Toggle` by fetching them.
pub fn folder_tree(ui: &mut Ui, tree: &FolderTree<'_>) -> Vec<TreeAction> {
    let mut actions = Vec::new();
    row(
        ui,
        &Row {
            path: "",
            name: tree.root_label,
            depth: 0,
            icon: Icon::FolderSync,
            chevron: None,
        },
        tree,
        &mut actions,
    );
    nodes(ui, tree.nodes, 0, tree, &mut actions);
    actions
}

fn nodes(
    ui: &mut Ui,
    list: &[FolderNode],
    depth: u16,
    tree: &FolderTree<'_>,
    actions: &mut Vec<TreeAction>,
) {
    for node in list {
        let open = tree.expanded.contains(&node.path);
        let chevron = match &node.children {
            NodeState::Loaded(children) if children.is_empty() => None,
            _ => Some(open),
        };
        row(
            ui,
            &Row {
                path: &node.path,
                name: &node.name,
                depth: depth + 1,
                icon: Icon::Folder,
                chevron: chevron.map(|open| (open, matches!(node.children, NodeState::Loading))),
            },
            tree,
            actions,
        );
        if open && let NodeState::Loaded(children) = &node.children {
            nodes(ui, children, depth + 1, tree, actions);
        }
    }
}

struct Row<'a> {
    path: &'a str,
    name: &'a str,
    depth: u16,
    icon: Icon,
    /// `(expanded, loading)`, absent for rows that cannot expand.
    chevron: Option<(bool, bool)>,
}

fn row(ui: &mut Ui, row: &Row<'_>, tree: &FolderTree<'_>, actions: &mut Vec<TreeAction>) {
    let palette = palette_of(ui);
    let selected = tree.selected == Some(row.path);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            row.name,
        )
    });
    let indent = f32::from(row.depth) * INDENT;
    let chevron_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + space::S + indent, rect.top()),
        Vec2::new(CHEVRON_SLOT, ROW_HEIGHT),
    );
    let chevron_response = row.chevron.map(|(open, _)| {
        let toggle = ui.interact(chevron_rect, response.id.with("chevron"), Sense::click());
        let verb = if open { "Collapse" } else { "Expand" };
        toggle.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("{verb} {}", row.name),
            )
        });
        toggle
    });
    if response.clicked() {
        actions.push(TreeAction::Select(row.path.to_owned()));
    }
    if response.double_clicked() && row.chevron.is_some() {
        actions.push(TreeAction::Toggle(row.path.to_owned()));
    }
    if chevron_response
        .as_ref()
        .is_some_and(egui::Response::clicked)
    {
        actions.push(TreeAction::Toggle(row.path.to_owned()));
    }
    if ui.is_rect_visible(rect) {
        if selected || response.hovered() {
            let fill = if selected {
                palette.surface_active
            } else {
                palette.surface_hover
            };
            ui.painter()
                .rect_filled(rect, f32::from(radius::CONTROL), fill);
        }
        if let Some((open, loading)) = row.chevron {
            if loading {
                ui.put(
                    egui::Rect::from_center_size(chevron_rect.center(), Vec2::splat(12.0)),
                    egui::Spinner::new().size(12.0).color(palette.dim),
                );
            } else {
                let icon = if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                };
                paint_icon(ui, icon, chevron_rect.center(), 12.0, palette.dim);
            }
        }
        let icon_color = if row.depth == 0 {
            palette.accent
        } else {
            palette.secondary
        };
        let icon_center = egui::pos2(chevron_rect.right() + size::ICON / 2.0, rect.center().y);
        paint_icon(ui, row.icon, icon_center, size::ICON, icon_color);
        let font = if row.depth == 0 {
            theme::headline()
        } else {
            theme::body()
        };
        let galley = ui
            .painter()
            .layout_no_wrap(row.name.to_owned(), font, palette.text);
        ui.painter().galley(
            egui::pos2(
                icon_center.x + size::ICON / 2.0 + space::S,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            palette.text,
        );
    }
    if let Some(toggle) = &chevron_response {
        focus_ring(ui, toggle, radius::CONTROL);
    }
    focus_ring(ui, &response, radius::CONTROL);
}
