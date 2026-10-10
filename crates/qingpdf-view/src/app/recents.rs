//! The files opened last (3d-2): the reader keeps where it was in the open file, and the list is in the File menu.

use super::*;
use crate::recent::{Entry, MODE_CUSTOM, MODE_FIT_PAGE, MODE_FIT_WIDTH};

impl App {
    /// Remember the page and zoom of the open file in the list (the file goes to the front), and keep the list.
    pub(super) fn remember_position(&mut self) {
        let Some(page) = self.visible().map(|v| v.0) else { return };
        // The speed test and the print test open files of their own: the person's list is not touched by them.
        if self.path.is_none() || self.bench.is_some() || self.print_test.is_some() {
            return;
        }
        let mode = match self.mode {
            ZoomMode::FitWidth => MODE_FIT_WIDTH,
            ZoomMode::FitPage => MODE_FIT_PAGE,
            ZoomMode::Custom => MODE_CUSTOM,
        };
        let path = self.path_text();
        recent::touch(&mut self.recent, Entry { path, page, zoom: self.zoom, mode });
        self.save_recent();
        self.menus_dirty = true;
    }

    /// Write the list to its file; a list that cannot be kept is no reason to stop.
    pub(super) fn save_recent(&self) {
        if let Some(path) = &self.recent_path {
            let _ = recent::save(path, &self.recent);
        }
    }

    pub(super) fn clear_recent(&mut self) -> Vec<Action> {
        self.recent.clear();
        self.save_recent();
        self.menus_dirty = true;
        Vec::new()
    }

    /// Open the `index`th file of the list (where it was left is restored when it is open).
    pub(super) fn open_recent(&mut self, index: usize) -> Vec<Action> {
        let Some(entry) = self.recent.get(index).cloned() else { return Vec::new() };
        self.open_path(Path::new(&entry.path), "")
    }
}
