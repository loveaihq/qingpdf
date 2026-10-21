//! Pages as pictures, and the pictures in pages (2026-10-21): File > Export as images... (a box with the pages, the format, the quality and the
//! resolution, then the folder) and File > Extract pictures from the pages... (the pages, then the folder). The engine writes the files a page at a
//! time and says how far it is; Esc stops it. Pages cannot be taken out or moved meanwhile (the engine counts them as they were when it began).

use qingpdf_core::export::{self, DEFAULT_QUALITY, DPI_CHOICES, ImageFormat};
use qingpdf_core::view::SaveStatus;

use super::cropping::{SCOPES, scope_pages};
use super::*;
use crate::i18n::fill;
use crate::ui::{FormAnswer, FormItem, FormKind::*, FormSpec};

/// What the folder is asked for.
pub(super) enum Job {
    Export { pages: Vec<u32>, dpi: f64, format: ImageFormat },
    Extract { pages: Vec<u32> },
}

pub(super) struct Pictures {
    /// The box is answered and the folder is being asked for.
    waiting: Option<Job>,
    /// Files with the names of the pages are in the folder: what is to be written, and the folder, while "replace them?" is asked.
    replacing: Option<(Job, String)>,
    /// The engine is writing: the request, and the folder.
    running: Option<(u64, String)>,
}

impl Pictures {
    pub(super) fn new() -> Pictures {
        Pictures { waiting: None, replacing: None, running: None }
    }

    pub(super) fn busy(&self) -> bool {
        self.waiting.is_some() || self.replacing.is_some() || self.running.is_some()
    }

    /// The document is gone: the request to take back.
    pub(super) fn reset(&mut self) -> Option<u64> {
        self.waiting = None;
        self.replacing = None;
        self.running.take().map(|r| r.0)
    }
}

impl App {
    /// The actions that say why pictures cannot be made now; `None` when they can.
    #[inline(never)]
    fn pictures_not_now(&mut self) -> Option<Vec<Action>> {
        if self.doc.is_none() {
            return Some(Vec::new());
        }
        if !self.rights.copy {
            return Some(self.say(t(self.lang, Msg::NoteCopyDenied).to_string()));
        }
        if self.ocr.is_some() || self.print.is_some() || self.px.busy() || self.cr.busy() || self.pages_busy() {
            return Some(self.say(t(self.lang, Msg::NoteBusy).to_string()));
        }
        None
    }

    /// File > Export as images...
    #[inline(never)]
    pub(super) fn ask_export_images(&mut self) -> Vec<Action> {
        if let Some(actions) = self.pictures_not_now() {
            return actions;
        }
        let lang = self.lang;
        let (scopes, scope) = self.scope_choices(3);
        let items = vec![
            FormItem::new(Label, t(lang, Msg::ApplyTo), (12, 12, 90, 18)),
            FormItem::pick(scopes, scope, (104, 8, 304, 120)),
            FormItem::new(Label, t(lang, Msg::ExpFormat), (12, 44, 90, 18)),
            FormItem::pick(vec![t(lang, Msg::ExpPng).to_string(), t(lang, Msg::ExpJpeg).to_string()], 0, (104, 40, 304, 100)),
            FormItem::new(Label, t(lang, Msg::ExpQuality), (12, 76, 190, 18)),
            FormItem::new(Edit, &DEFAULT_QUALITY.to_string(), (204, 73, 80, 22)),
            FormItem::new(Label, t(lang, Msg::ExpDpi), (12, 108, 90, 18)),
            FormItem::pick(DPI_CHOICES.iter().map(|d| format!("{d} dpi")).collect(), 1, (104, 104, 140, 100)),
            FormItem::new(Label, t(lang, Msg::ExpNote), (12, 140, 396, 52)),
        ];
        vec![Action::AskForm(FormSpec { tag: TAG_EXPORT_IMAGES, title: t(lang, Msg::ExpTitle).to_string(), size: (420, 244), items, ok: t(lang, Msg::Ok).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None })]
    }

    /// File > Extract pictures from the pages...
    #[inline(never)]
    pub(super) fn ask_extract_images(&mut self) -> Vec<Action> {
        if let Some(actions) = self.pictures_not_now() {
            return actions;
        }
        let lang = self.lang;
        let items = vec![
            FormItem::new(Label, t(lang, Msg::ExtIntro), (12, 8, 396, 98)),
            FormItem::new(Label, t(lang, Msg::ApplyTo), (12, 118, 90, 18)),
            FormItem::pick(self.scope_choices(3).0, 2, (104, 114, 304, 120)),
        ];
        vec![Action::AskForm(FormSpec { tag: TAG_EXTRACT_IMAGES, title: t(lang, Msg::ExtTitle).to_string(), size: (420, 172), items, ok: t(lang, Msg::Ok).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None })]
    }

    /// The pages a box's first choice names (the first three of the list: this page, the chosen ones, all).
    #[inline(never)]
    fn picture_pages(&mut self, which: &str) -> Option<Vec<u32>> {
        let scope = SCOPES.get(which.parse::<usize>().ok()?.min(2))?.0;
        scope_pages(scope, self.thumb_current(), &self.pg.chosen.pages(), self.page_count(), "")
    }

    #[inline(never)]
    pub(super) fn export_images_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        // (The pages, the format, the quality and the resolution.)
        let Some(FormAnswer { values, .. }) = answer.filter(|a| a.values.len() == 4) else { return Vec::new() };
        if self.pictures_not_now().is_some_and(|a| !a.is_empty()) {
            return Vec::new();
        }
        let [which, format, quality, dpi] = <[String; 4]>::try_from(values).unwrap_or_default();
        let Some(pages) = self.picture_pages(&which) else { return Vec::new() };
        let format = if format == "1" {
            ImageFormat::Jpeg(quality.trim().parse::<u8>().ok().filter(|q| (1..=100).contains(q)).unwrap_or(DEFAULT_QUALITY))
        } else {
            ImageFormat::Png
        };
        let dpi = DPI_CHOICES.get(dpi.parse::<usize>().unwrap_or(1)).map_or(150.0, |d| f64::from(*d));
        self.px.waiting = Some(Job::Export { pages, dpi, format });
        vec![Action::PickFolder { title: t(self.lang, Msg::ExpPick).to_string() }]
    }

    #[inline(never)]
    pub(super) fn extract_images_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(FormAnswer { values, .. }) = answer.filter(|a| a.values.len() == 1) else { return Vec::new() };
        if self.pictures_not_now().is_some_and(|a| !a.is_empty()) {
            return Vec::new();
        }
        let Some(pages) = values.first().and_then(|w| self.picture_pages(w)) else { return Vec::new() };
        self.px.waiting = Some(Job::Extract { pages });
        vec![Action::PickFolder { title: t(self.lang, Msg::ExpPick).to_string() }]
    }

    /// The name pictures start with: the name of the file.
    fn picture_stem(&self) -> String {
        export::file_stem(&self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
    }

    /// The box that picks the folder was answered. `None`: it was not asked for pictures. Pages as pictures that would replace files that are in the
    /// folder are asked about first (once, for all of them); pictures taken out of pages never replace a file (the engine puts -2, -3 ... in the name).
    #[inline(never)]
    pub(super) fn folder_picked(&mut self, dir: Option<PathBuf>) -> Option<Vec<Action>> {
        let job = self.px.waiting.take()?;
        let dir = dir.map(|d| std::path::absolute(&d).unwrap_or(d))?;
        if self.doc.is_none() {
            return Some(Vec::new());
        }
        let dir_text = dir.to_string_lossy().into_owned();
        if let Job::Export { pages, format, .. } = &job {
            let stem = self.picture_stem();
            let there: Vec<String> = pages.iter().map(|&p| export::page_file_name(&stem, p as usize, *format)).filter(|n| std::fs::symlink_metadata(dir.join(n)).is_ok()).collect();
            if let Some(first) = there.first() {
                let text = fill(t(self.lang, Msg::ExpReplace), &[("n", &there.len().to_string()), ("name", first)]);
                self.px.replacing = Some((job, dir_text));
                return Some(vec![Action::Confirm { tag: TAG_EXPORT_REPLACE, title: t(self.lang, Msg::ExpTitle).to_string(), text }]);
            }
        }
        Some(self.start_pictures(job, dir_text))
    }

    /// The answer to "replace them?".
    #[inline(never)]
    pub(super) fn export_replace_answered(&mut self, yes: bool) -> Vec<Action> {
        match self.px.replacing.take() {
            Some((job, dir)) if yes && self.doc.is_some() => self.start_pictures(job, dir),
            _ => Vec::new(),
        }
    }

    /// Ask the engine to write the files in the folder.
    #[inline(never)]
    fn start_pictures(&mut self, job: Job, dir: String) -> Vec<Action> {
        let (id, stem) = (self.new_id(), self.picture_stem());
        match job {
            Job::Export { pages, dpi, format } => self.engine.export_images(self.doc_id, id, &dir, &stem, pages, (dpi, format)),
            Job::Extract { pages } => self.engine.extract_images(self.doc_id, id, &dir, &stem, pages),
        }
        self.px.running = Some((id, dir));
        self.say(fill(t(self.lang, Msg::NotePictures), &[("n", "0")]))
    }

    /// How far the engine is. `None`: that request is not the pictures.
    #[inline(never)]
    pub(super) fn on_pictures_progress(&mut self, id: u64, percent: u32) -> Option<Vec<Action>> {
        (self.px.running.as_ref().is_some_and(|r| r.0 == id)).then(|| self.say(fill(t(self.lang, Msg::NotePictures), &[("n", &percent.to_string())])))
    }

    /// Esc while the files are being written. `None`: they are not.
    pub(super) fn cancel_pictures(&mut self) -> Option<Vec<Action>> {
        let (id, _) = self.px.running.take()?;
        self.engine.cancel(id);
        Some(self.say(t(self.lang, Msg::NoteStopped).to_string()))
    }

    /// The engine has written the files (or not). `None`: that request was not the pictures.
    #[inline(never)]
    pub(super) fn on_pictures_done(&mut self, id: u64, status: SaveStatus, message: &str, files: u32) -> Option<Vec<Action>> {
        let (_, dir) = self.px.running.take().filter(|r| r.0 == id)?;
        let lang = self.lang;
        let text = match status {
            SaveStatus::Failed => format!("{}\n{message}", t(lang, Msg::ExpFailed)),
            SaveStatus::Done if files == 0 && message.is_empty() => t(lang, Msg::ExpNone).to_string(),
            SaveStatus::Done => {
                let done = fill(t(lang, Msg::ExpDone), &[("n", &files.to_string()), ("dir", &dir)]);
                if message.is_empty() { done } else { format!("{done}\n\n{}\n{message}", t(lang, Msg::ExpFailed)) }
            }
        };
        Some(vec![Action::Message { title: t(lang, Msg::ExpTitle).to_string(), text }])
    }
}
