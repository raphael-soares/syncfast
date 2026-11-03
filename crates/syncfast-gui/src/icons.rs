use syncfast_core::state::DisplayStatus;

fastframe_icons::icons! {
    /// Every icon the interface draws. Shared Lucide icons come from
    /// fastframe-icons; the rest are Lucide files copied to `assets/icons/`.
    pub enum Icon {
        prefix: "syncfast-icon-",
        directory: "../assets/icons/",
        Check => lucide "check",
        ChevronDown => lucide "chevron-down",
        ChevronRight => lucide "chevron-right",
        CircleAlert => lucide "circle-alert",
        Clock => lucide "clock",
        CloudOff => "cloud-off",
        Copy => lucide "copy",
        Ellipsis => lucide "ellipsis",
        ExternalLink => lucide "external-link",
        Eye => lucide "eye",
        Filter => "funnel",
        Folder => "folder",
        FolderPlus => "folder-plus",
        FolderSync => "folder-sync",
        History => "history",
        Palette => "palette",
        Pause => lucide "pause",
        Play => lucide "play",
        Plus => lucide "plus",
        Refresh => lucide "refresh-cw",
        Settings => lucide "settings",
        Trash => lucide "trash-2",
        User => lucide "user",
        X => lucide "x",
    }
}

/// The icon that accompanies a status, so colour is never the only signal.
#[must_use]
pub fn status_icon(status: DisplayStatus) -> Icon {
    match status {
        DisplayStatus::Syncing => Icon::Refresh,
        DisplayStatus::Error | DisplayStatus::Failing => Icon::CircleAlert,
        DisplayStatus::NeedsReview => Icon::Eye,
        DisplayStatus::Conflicts(_) => Icon::Copy,
        DisplayStatus::Paused => Icon::Pause,
        DisplayStatus::Offline => Icon::CloudOff,
        DisplayStatus::Retrying => Icon::Clock,
        DisplayStatus::UpToDate => Icon::Check,
    }
}

/// The icon's raw SVG, for rasterizers outside egui such as the tray.
#[must_use]
pub fn svg(icon: Icon) -> &'static str {
    std::str::from_utf8(icon.bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_icon_matches_the_name_core_assigns() {
        let statuses = [
            DisplayStatus::Syncing,
            DisplayStatus::Error,
            DisplayStatus::NeedsReview,
            DisplayStatus::Conflicts(2),
            DisplayStatus::Paused,
            DisplayStatus::Offline,
            DisplayStatus::Failing,
            DisplayStatus::Retrying,
            DisplayStatus::UpToDate,
        ];
        for status in statuses {
            let name = status.icon();
            let uri = status_icon(status).uri();
            assert!(
                uri.ends_with(&format!("{name}.svg")),
                "{status:?}: {uri} does not draw {name}"
            );
        }
    }

    #[test]
    fn tray_glyphs_are_complete_svg_documents() {
        for icon in [Icon::FolderSync, Icon::Refresh, Icon::CloudOff] {
            let svg = svg(icon);
            assert!(svg.contains("<svg") && svg.contains("</svg>"), "{icon:?}");
        }
    }
}
