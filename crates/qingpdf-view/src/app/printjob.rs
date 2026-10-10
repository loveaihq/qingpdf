//! Printing (3d-2): which pages, how they sit on the paper, the bands the engine draws one at a time and the printer's side of them,
//! progress and cancelling; and the hidden `--print-test`, which prints to a printer that writes a file with no box.

use std::path::PathBuf;

use qingpdf_core::view::{PrintBand, PrintPage, PrintStatus};

use super::*;
use crate::printing::{self, Fit};
use crate::ui::{PrintOp, PrinterPreset, PrinterSetup};

/// A print job the engine is working on.
pub(super) struct PrintJob {
    /// The engine's request.
    id: u64,
    setup: PrinterSetup,
    /// How each page of the job sits on the paper (in the order of the job).
    fits: Vec<Fit>,
    total: usize,
    /// Bands put on pages so far, for the test.
    bands: usize,
}

/// What the hidden `--print-test <file> <out.pdf> [first-last]` asks for: print these pages (from 1) to "Microsoft Print to PDF" that
/// writes `output`, with no box, say how it went and quit.
pub struct PrintTest {
    pub output: PathBuf,
    pub first: u32,
    pub last: u32,
}

/// The printer that writes a PDF file, which comes with Windows.
const PDF_PRINTER: &str = "Microsoft Print to PDF";

impl App {
    pub fn set_print_test(&mut self, test: PrintTest) {
        self.print_test = Some(test);
    }

    /// After a file has opened for the test: print it.
    pub(super) fn start_print_test(&mut self) -> Vec<Action> {
        let Some(test) = &self.print_test else { return Vec::new() };
        let preset = PrinterPreset { name: PDF_PRINTER.to_string(), output: test.output.clone(), ranges: vec![(test.first, test.last)] };
        vec![Action::ChoosePrinter { max_page: self.page_count() as u32, preset: Some(preset) }]
    }

    /// Ctrl+P or the menu: the system's print box, if the file allows printing.
    pub(super) fn print_command(&mut self) -> Vec<Action> {
        if self.doc.is_none() || self.print.is_some() {
            return Vec::new();
        }
        if !self.rights.print {
            return self.say(t(self.lang, Msg::NotePrintDenied).to_string());
        }
        vec![Action::ChoosePrinter { max_page: self.page_count() as u32, preset: None }]
    }

    /// The printer and pages are chosen: start the job.
    pub(super) fn printer_chosen(&mut self, setup: Option<PrinterSetup>) -> Vec<Action> {
        let Some(setup) = setup else {
            if self.print_test.is_some() {
                return self.finish_test("no printer called \"Microsoft Print to PDF\" could be opened");
            }
            // The box was cancelled, or there is no printer at all (the box would have said so).
            return Vec::new();
        };
        let Some(doc) = &self.doc else { return vec![Action::Print(PrintOp::Abort)] };
        if !self.rights.print || self.print.is_some() {
            return vec![Action::Print(PrintOp::Abort)];
        }
        let pages = printing::pages_of(&setup.ranges, doc.layout.len() as u32);
        if pages.is_empty() {
            return vec![Action::Print(PrintOp::Abort)];
        }
        let mut list = Vec::with_capacity(pages.len());
        let mut fits = Vec::with_capacity(pages.len());
        for &page in &pages {
            let (w, h) = doc.layout.page_size_pt(page as usize).unwrap_or((612.0, 792.0));
            let fit = printing::fit(w, h, setup.width, setup.height, setup.dpi);
            list.push(PrintPage { page, dpi: fit.dpi, rotation: fit.rotation });
            fits.push(fit);
        }
        let total = list.len();
        let id = self.new_id();
        self.engine.print(self.doc_id, id, list);
        let name = self.path.as_ref().and_then(|p| p.file_name()).map_or_else(|| "qingpdf".to_string(), |n| n.to_string_lossy().into_owned());
        self.note = Some(if self.rights.print_high_quality { i18n::printing(self.lang, 1, total) } else { t(self.lang, Msg::NotePrintLow).to_string() });
        self.print = Some(PrintJob { id, setup, fits, total, bands: 0 });
        vec![Action::Print(PrintOp::Start { name }), Action::Invalidate]
    }

    /// A band is ready: put it on the page.
    pub(super) fn on_print_band(&mut self, b: PrintBand) -> Vec<Action> {
        let Some(job) = self.print.as_mut().filter(|j| j.id == b.id) else { return Vec::new() };
        let Some(fit) = job.fits.get(b.index as usize).copied() else { return Vec::new() };
        // By the dpi the engine really drew at, not the one that was asked for: it may have held it lower (low quality printing).
        let factor = printing::pixel_factor(fit, b.dpi);
        let origin = printing::origin(b.page_width, b.page_height, factor, job.setup.width, job.setup.height);
        let (x, y, w, h) = printing::band_dest(origin, factor, b.y, b.width, b.height);
        job.bands += 1;
        let note = i18n::printing(self.lang, b.index as usize + 1, job.total);
        let mut actions = Vec::new();
        if b.y == 0 {
            actions.push(Action::Print(PrintOp::StartPage));
        }
        let last_band = u64::from(b.y) + u64::from(b.height) >= u64::from(b.page_height);
        actions.push(Action::Print(PrintOp::Band { x, y, w, h, src_w: b.width as i32, src_h: b.height as i32, bgra: b.bgra }));
        if last_band {
            actions.push(Action::Print(PrintOp::EndPage));
        }
        self.note = Some(note);
        actions.push(Action::Invalidate);
        actions
    }

    /// The band has been put on the page: the engine may draw the next.
    pub(super) fn print_step_done(&mut self) -> Vec<Action> {
        if let Some(job) = &self.print {
            self.engine.print_next(job.id);
        }
        Vec::new()
    }

    pub(super) fn on_print_done(&mut self, id: u64, status: PrintStatus, message: &str) -> Vec<Action> {
        let Some(job) = self.print.take().filter(|j| j.id == id) else { return Vec::new() };
        self.note = None;
        let lang = self.lang;
        match status {
            PrintStatus::Finished => {
                let mut actions = vec![Action::Print(PrintOp::End)];
                if self.print_test.is_some() {
                    actions.extend(self.finish_test(&format!("ok, {} pages, {} bands", job.total, job.bands)));
                } else {
                    actions.extend(self.say(t(lang, Msg::NotePrinted).to_string()));
                }
                actions
            }
            PrintStatus::Failed => {
                let mut actions = vec![Action::Print(PrintOp::Abort)];
                if self.print_test.is_some() {
                    actions.extend(self.finish_test(&format!("failed: {message}")));
                } else {
                    actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::PrintFailed)) });
                }
                actions
            }
        }
    }

    /// The printer refused something: stop the job and say so.
    pub(super) fn print_error(&mut self, message: &str) -> Vec<Action> {
        let mut actions = self.cancel_print_for_new_document();
        if self.print_test.is_some() {
            actions.extend(self.finish_test(&format!("failed: the printer refused: {message}")));
            return actions;
        }
        // No message: there is no printer to print to (the system's box would not open).
        let text = if message.is_empty() { t(self.lang, Msg::NoPrinter).to_string() } else { format!("{}\n{message}", t(self.lang, Msg::PrintFailed)) };
        actions.push(Action::Message { title: t(self.lang, Msg::Error).to_string(), text });
        actions
    }

    /// Stop the job that is going (if any), in the engine and at the printer.
    pub(super) fn cancel_print_for_new_document(&mut self) -> Vec<Action> {
        match self.print.take() {
            Some(job) => {
                self.engine.cancel(job.id);
                self.note = None;
                vec![Action::Print(PrintOp::Abort), Action::Invalidate]
            }
            None => Vec::new(),
        }
    }

    /// The end of `--print-test`: say how it went (and how much memory the process used at most) and quit.
    fn finish_test(&mut self, result: &str) -> Vec<Action> {
        println!("print-test: {result}");
        println!("print-test: peak working set {:.1} MB", crate::win32::peak_working_set() as f64 / 1_000_000.0);
        vec![Action::Quit]
    }
}
