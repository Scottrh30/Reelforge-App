//! The dock layout, and the Tabs menu that shows or hides each window.

use crate::app::Tab;
use egui::{Color32, CursorIcon, Pos2, Rect, Stroke, Vec2};
use egui_dock::{DockState, Node, NodeIndex, Style};

/// Thickness of the divider bars between panels, and how far either side of
/// one still grabs it.
pub const DIVIDER: f32 = 6.0;
const DIVIDER_GRAB: f32 = 6.0;
/// The smallest a panel can be dragged to (points).
const MIN_PANEL: f32 = 70.0;

/// The dock's look: tabs from the egui theme, and divider bars you can see and grab.
pub fn dock_style(egui_style: &egui::Style) -> Style {
    let mut style = Style::from_egui(egui_style);
    let accent = egui_style.visuals.selection.bg_fill;
    style.separator.width = DIVIDER;
    style.separator.extra_interact_width = DIVIDER_GRAB;
    style.separator.extra = MIN_PANEL;
    style.separator.color_idle = Color32::from_gray(38);
    style.separator.color_hovered = accent.gamma_multiply(0.75);
    style.separator.color_dragged = accent;
    style
}

/// Grip marks in the middle of each divider bar, so they read as handles.
/// Drawn over the dock after it's shown.
pub fn paint_dividers(ui: &egui::Ui, dock: &DockState<Tab>) {
    let painter = ui.painter();
    let pointer = ui.ctx().pointer_hover_pos();
    let dragging = ui.ctx().dragged_id().is_some();
    for node in dock.main_surface().iter() {
        let (split, across) = match node {
            Node::Horizontal(s) => (s, true), // side by side: a vertical bar
            Node::Vertical(s) => (s, false),  // one above the other: a horizontal bar
            _ => continue,
        };
        let r = split.rect;
        let bar = if across {
            let x = r.min.x + r.width() * split.fraction;
            Rect::from_min_max(Pos2::new(x - DIVIDER / 2.0, r.min.y), Pos2::new(x + DIVIDER / 2.0, r.max.y))
        } else {
            let y = r.min.y + r.height() * split.fraction;
            Rect::from_min_max(Pos2::new(r.min.x, y - DIVIDER / 2.0), Pos2::new(r.max.x, y + DIVIDER / 2.0))
        };
        if !bar.is_positive() {
            continue;
        }
        let hot = !dragging && pointer.is_some_and(|p| bar.expand(DIVIDER_GRAB / 2.0).contains(p));
        if hot {
            ui.ctx().set_cursor_icon(if across { CursorIcon::ResizeHorizontal } else { CursorIcon::ResizeVertical });
        }
        let dot = if hot { Color32::from_gray(230) } else { Color32::from_gray(110) };
        let c = bar.center();
        let step = if across { Vec2::new(0.0, 5.0) } else { Vec2::new(5.0, 0.0) };
        for i in -2..=2 {
            painter.circle_filled(c + step * i as f32, 1.1, dot);
        }
        // A hairline on each edge keeps neighbouring panels apart visually.
        let edge = Stroke::new(1.0, Color32::from_gray(24));
        if across {
            painter.vline(bar.min.x, bar.y_range(), edge);
            painter.vline(bar.max.x, bar.y_range(), edge);
        } else {
            painter.hline(bar.x_range(), bar.min.y, edge);
            painter.hline(bar.x_range(), bar.max.y, edge);
        }
    }
}

/// Where the layout (panel sizes and arrangement) is kept between runs.
pub const STORAGE_KEY: &str = "reelforge_layout";

/// A saved layout, if it's usable: every tab at most once.
pub fn checked_layout(dock: DockState<Tab>) -> Option<DockState<Tab>> {
    let tabs: Vec<Tab> = dock.iter_all_tabs().map(|(_, t)| *t).collect();
    let unique = tabs.iter().enumerate().all(|(i, t)| !tabs[..i].contains(t));
    (unique && !tabs.is_empty()).then_some(dock)
}

/// Every window, in the order the Tabs menu lists them, with a hint.
pub const ALL: [(Tab, &str); 8] = [
    (Tab::Viewer, "The picture at the playhead, with the transport controls"),
    (Tab::MediaBin, "Imported files, titles and compositions"),
    (Tab::History, "Every edit, to step back to any point"),
    (Tab::Inspector, "Controls for the selected clip and the sequence"),
    (Tab::Timeline, "The main edit"),
    (Tab::Compositions, "The composition you're working on"),
    (Tab::Curves, "Keyframe graphs of the selected clip"),
    (Tab::Meters, "Audio levels"),
];

/// The name shown on the tab and in the Tabs menu.
pub fn title(tab: Tab) -> &'static str {
    match tab {
        Tab::MediaBin => "Media",
        Tab::Viewer => "Viewer",
        Tab::Inspector => "Controls",
        Tab::Timeline => "Editor Timeline",
        Tab::Compositions => "Composition Timeline",
        Tab::Curves => "Curves",
        Tab::Meters => "Meters",
        Tab::History => "History",
    }
}

/// HitFilm-like layout: media bin left, viewer centre, controls right, timelines across the bottom.
pub fn default_layout() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Viewer]);
    let tree = dock.main_surface_mut();
    // `fraction` is the share kept by the original node (the viewer).
    let [top, timeline] = tree.split_below(NodeIndex::root(), 0.6, vec![Tab::Timeline, Tab::Compositions, Tab::Curves]);
    tree.split_right(timeline, 0.93, vec![Tab::Meters]);
    let [viewer, _bin] = tree.split_left(top, 0.18, vec![Tab::MediaBin, Tab::History]);
    tree.split_right(viewer, 0.76, vec![Tab::Inspector]);
    dock
}

/// Windows that share a panel in the default layout (a hidden window comes
/// back next to one of these if it can).
fn neighbours(tab: Tab) -> &'static [Tab] {
    match tab {
        Tab::Timeline | Tab::Compositions | Tab::Curves => &[Tab::Timeline, Tab::Compositions, Tab::Curves],
        Tab::MediaBin | Tab::History => &[Tab::MediaBin, Tab::History],
        Tab::Viewer => &[Tab::Viewer],
        Tab::Inspector => &[Tab::Inspector],
        Tab::Meters => &[Tab::Meters],
    }
}

#[cfg(test)]
fn is_shown(dock: &DockState<Tab>, tab: Tab) -> bool {
    dock.find_tab(&tab).is_some()
}

/// Take a window out of the layout (its panel closes if it was the last one there).
pub fn hide(dock: &mut DockState<Tab>, tab: Tab) {
    if let Some(path) = dock.find_tab(&tab) {
        dock.remove_tab(path);
    }
}

/// Put a window back (beside the windows it normally shares a panel with, or
/// where it normally sits), or bring it to the front if it's already shown.
pub fn show(dock: &mut DockState<Tab>, tab: Tab) {
    if dock.find_tab(&tab).is_none() {
        let beside = neighbours(tab).iter().filter(|t| **t != tab).find_map(|t| dock.find_tab(t));
        match beside {
            Some(path) => dock[path.surface][path.node].append_tab(tab),
            None if dock.main_surface().is_empty() => dock.push_to_first_leaf(tab),
            None => {
                // Its own panel, split off the one it normally sits beside.
                let find = |tabs: &[Tab]| tabs.iter().find_map(|t| dock.find_main_surface_tab(t)).map(|(n, _)| n);
                let timelines = find(&[Tab::Timeline, Tab::Compositions, Tab::Curves]);
                let viewer = find(&[Tab::Viewer]);
                let media = find(&[Tab::MediaBin, Tab::History]);
                let controls = find(&[Tab::Inspector]);
                let root = NodeIndex::root();
                let tree = dock.main_surface_mut();
                let tabs = vec![tab];
                match tab {
                    Tab::Timeline | Tab::Compositions | Tab::Curves => {
                        tree.split_below(root, 0.6, tabs);
                    }
                    Tab::Meters => {
                        tree.split_right(timelines.unwrap_or(root), 0.93, tabs);
                    }
                    Tab::MediaBin | Tab::History => {
                        tree.split_left(viewer.or(controls).unwrap_or(root), 0.2, tabs);
                    }
                    Tab::Inspector => {
                        tree.split_right(viewer.or(media).unwrap_or(root), 0.76, tabs);
                    }
                    Tab::Viewer => match (media, controls) {
                        (Some(m), _) => {
                            tree.split_right(m, 0.2, tabs);
                        }
                        (None, Some(c)) => {
                            tree.split_left(c, 0.75, tabs);
                        }
                        (None, None) => {
                            tree.split_above(root, 0.4, tabs);
                        }
                    },
                }
            }
        }
    }
    if let Some(path) = dock.find_tab(&tab) {
        let _ = dock.set_active_tab(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_round_trips_through_storage() {
        let mut dock = default_layout();
        hide(&mut dock, Tab::Meters);
        if let Node::Vertical(s) = &mut dock.main_surface_mut()[NodeIndex::root()] {
            s.fraction = 0.42;
        }
        let text = ron::to_string(&dock).unwrap();
        let back: DockState<Tab> = checked_layout(ron::from_str(&text).unwrap()).unwrap();
        assert!(!is_shown(&back, Tab::Meters) && is_shown(&back, Tab::Viewer));
        match &back.main_surface()[NodeIndex::root()] {
            Node::Vertical(s) => assert_eq!(s.fraction, 0.42),
            _ => panic!("root should still be the viewer/timeline split"),
        }
        // A layout with a window twice isn't used.
        let mut twice = default_layout();
        twice.push_to_first_leaf(Tab::Viewer);
        assert!(checked_layout(twice).is_none());
    }

    fn tabs(dock: &DockState<Tab>) -> Vec<Tab> {
        dock.iter_all_tabs().map(|(_, t)| *t).collect()
    }

    #[test]
    fn hide_and_show_every_window() {
        let mut dock = default_layout();
        assert_eq!(tabs(&dock).len(), ALL.len());
        for (tab, _) in ALL.iter() {
            hide(&mut dock, *tab);
            assert!(!is_shown(&dock, *tab));
        }
        assert!(tabs(&dock).is_empty());
        // Back one by one, each lands somewhere and the others stay.
        for (n, (tab, _)) in ALL.iter().enumerate() {
            show(&mut dock, *tab);
            assert!(is_shown(&dock, *tab));
            assert_eq!(tabs(&dock).len(), n + 1);
        }
    }

    #[test]
    fn a_lone_window_comes_back_in_its_usual_place() {
        // Meters returns to the right of the timelines, not as a full-height column.
        let mut dock = default_layout();
        hide(&mut dock, Tab::Meters);
        show(&mut dock, Tab::Meters);
        let tree = dock.main_surface();
        let (meters, _) = tree.find_tab(&Tab::Meters).unwrap();
        let (timeline, _) = tree.find_tab(&Tab::Timeline).unwrap();
        let parent = |n: NodeIndex| n.parent().unwrap();
        assert_eq!(parent(meters), parent(timeline));
    }

    #[test]
    fn a_hidden_window_comes_back_beside_its_neighbours() {
        let mut dock = default_layout();
        hide(&mut dock, Tab::Compositions);
        show(&mut dock, Tab::Compositions);
        let node = |t: Tab| dock.find_tab(&t).map(|p| (p.surface, p.node));
        assert_eq!(node(Tab::Compositions), node(Tab::Timeline));
        // Showing a window that's already there doesn't duplicate it.
        show(&mut dock, Tab::Timeline);
        assert_eq!(tabs(&dock).iter().filter(|t| **t == Tab::Timeline).count(), 1);
    }
}
