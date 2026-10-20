//! File > Passwords and permissions: one box that shows how the open file is protected (the method, whether a password opens it, which
//! rights its author took away), and sets or changes the passwords, the rights and the method (AES-256 or AES-128), or takes the
//! passwords off. Then the person says where to write the result (the open file by default: the Windows box asks before it replaces a
//! file) and the engine writes the whole document again, with the changes not yet saved, under the new protection; the result is opened.
//! The passwords go from the box to the engine and nowhere else: not into a setting, a note or a message, and every copy is overwritten.

use qingpdf_core::security::{Method, PasswordProblem, Permissions, Protection, password_bytes};
use qingpdf_core::view::{Cipher, ProtectChange, Protected, SaveStatus};

use super::*;
use crate::ui::{FormAnswer, FormItem, FormKind, FormKind::*, FormSpec};

/// The things in the box, in the order they are made in: what each is, its words, and where it is (x, y, width, height). The things that
/// are filled in are, in order: the password to open and its repetition, the permissions password and its repetition, the method, the
/// printing, and the six rights that can be turned off.
#[rustfmt::skip]
const BOX: [(FormKind, Option<Msg>, [i32; 4]); 24] = [
    (Label, None, [10, 6, 540, 18]), (Label, None, [10, 26, 540, 18]), (Label, None, [10, 46, 540, 34]), (Label, None, [10, 84, 540, 34]),
    (Label, Some(Msg::PtOpenPw), [10, 126, 262, 18]), (Secret, None, [10, 146, 262, 24]), (Label, Some(Msg::PtAgain), [284, 126, 262, 18]), (Secret, None, [284, 146, 262, 24]),
    (Label, Some(Msg::PtOwnerPw), [10, 178, 262, 18]), (Secret, None, [10, 198, 262, 24]), (Label, Some(Msg::PtAgain), [284, 178, 262, 18]), (Secret, None, [284, 198, 262, 24]),
    (Label, Some(Msg::PtMethod), [10, 240, 80, 18]), (Pick, None, [92, 236, 200, 150]), (Label, Some(Msg::PtPrintLabel), [310, 240, 50, 18]), (Pick, None, [362, 236, 188, 150]),
    (Group, Some(Msg::PtAllow), [10, 268, 540, 100]),
    (Check, Some(Msg::PtCopy), [24, 290, 250, 22]), (Check, Some(Msg::PtModify), [290, 290, 250, 22]),
    (Check, Some(Msg::PtAnnotate), [24, 316, 250, 22]), (Check, Some(Msg::PtForms), [290, 316, 250, 22]),
    (Check, Some(Msg::PtAssemble), [24, 342, 250, 22]), (Check, Some(Msg::PtAccess), [290, 342, 250, 22]),
    (Button, Some(Msg::PtRemove), [10, 380, 170, 28]),
];

/// How many things the box fills in: four passwords, two lists, six tick boxes.
const VALUES: usize = 12;

/// The words of a right that a file may take away, in the order of the tick boxes.
const RIGHTS: [Msg; 6] = [Msg::PtCopy, Msg::PtModify, Msg::PtAnnotate, Msg::PtForms, Msg::PtAssemble, Msg::PtAccess];

pub(super) struct Protect {
    /// How the open file is protected, told by the engine when it is opened.
    pub info: Option<Protected>,
    /// Where the random numbers come from (Windows' generator; the engine has none of its own).
    pub random: fn(&mut [u8]) -> bool,
    /// What the person chose, waiting for the name of the file to write, and the password the result is opened with.
    waiting: Option<(ProtectChange, String)>,
    /// The engine is writing the file: the request, the file, and the password the result is opened with.
    running: Option<(u64, PathBuf, String)>,
    /// The choices of the box last used (the passwords are not kept), for showing it again after a mistake.
    last: Option<Vec<String>>,
}

impl Protect {
    pub(super) fn new() -> Protect {
        Protect { info: None, random: |_| false, waiting: None, running: None, last: None }
    }

    /// The open file has gone: nothing is waiting for it, and the request that was writing it (its answer is not wanted: it is for a document
    /// that is not open any more) takes its password with it.
    /// A request to change the protection is being made or waits for a file name.
    pub(super) fn is_busy(&self) -> bool {
        self.running.is_some() || self.waiting.is_some()
    }

    pub(super) fn reset(&mut self) {
        self.info = None;
        self.last = None;
        self.forget_waiting();
        if let Some((_, _, mut pw)) = self.running.take() {
            wipe(&mut pw);
        }
    }

    fn forget_waiting(&mut self) {
        if let Some((_, mut pw)) = self.waiting.take() {
            wipe(&mut pw);
        }
    }
}

/// Overwrite a password that is no longer needed.
fn wipe(text: &mut String) {
    qingpdf_core::security::wipe(text);
}

impl App {
    /// The program says where random numbers come from (the system's generator).
    pub fn set_random(&mut self, random: fn(&mut [u8]) -> bool) {
        self.pt.random = random;
    }

    /// The words for what the file's flags take away, in a list.
    fn denied_list(&self, p: &Permissions) -> String {
        let lang = self.lang;
        let mut names: Vec<&str> = Vec::new();
        if !p.print {
            names.push(t(lang, Msg::PtPrintLabel));
        }
        for (m, allowed) in RIGHTS.iter().zip([p.copy, p.modify, p.annotate, p.fill_forms, p.assemble, p.accessibility]) {
            if !allowed {
                names.push(t(lang, *m));
            }
        }
        names.join(if lang == Lang::Zh { "、" } else { ", " })
    }

    /// The box, with the file's protection as the start (or the choices of the last time after a mistake).
    fn protect_form(&self, info: &Protected) -> FormSpec {
        let lang = self.lang;
        let cipher_name = match info.cipher {
            None => String::new(),
            Some(Cipher::Aes256) => "AES-256".to_string(),
            Some(Cipher::Aes128) => "AES-128".to_string(),
            Some(Cipher::Rc4) => "RC4".to_string(),
            Some(Cipher::Other) => "RC4/AES".to_string(),
        };
        // How the file is protected, in words: the method, whether a password opens it, what it does not allow, and what is in the way.
        let state = [
            if info.cipher.is_some() { t(lang, Msg::PtCipher).replace("{c}", &cipher_name) } else { t(lang, Msg::PtNone).to_string() },
            String::from(if info.needs_password { t(lang, Msg::PtNeedsPw) } else { "" }),
            if info.cipher.is_some() && !info.permissions.allows_everything() { t(lang, Msg::PtDenied).replace("{list}", &self.denied_list(&info.permissions)) } else { String::new() },
            [(info.signed, Msg::PtSigned), (!info.may_change, Msg::PtOwnerNeeded)]
                .into_iter()
                .find(|(on, _)| *on)
                .map_or(String::new(), |(_, m)| t(lang, m).to_string()),
        ];
        let mut lines = state.into_iter();
        let p = &info.permissions;
        // What each thing that is filled in starts as: passwords empty; the method as the file has it; printing as the file allows it;
        // the rights as the file gives them.
        let start: Vec<String> = match &self.pt.last {
            Some(last) => last.clone(),
            None => {
                let print = if p.print && p.print_high_quality { 0 } else if p.print { 1 } else { 2 };
                let mut v: Vec<String> = vec![String::new(); 4];
                v.push(usize::from(info.cipher == Some(Cipher::Aes128)).to_string());
                v.push(print.to_string());
                v.extend([p.copy, p.modify, p.annotate, p.fill_forms, p.assemble, p.accessibility].map(|b| String::from(if b { "1" } else { "0" })));
                v
            }
        };
        let mut start = start.into_iter();
        let mut picks = 0usize;
        // The things without words of their own are the four lines that say how the file is protected, in order.
        let items = BOX
            .iter()
            .filter(|(kind, ..)| *kind != Button || info.cipher.is_some())
            .map(|&(kind, words, [x, y, w, h])| {
                let mut item = FormItem { kind, text: words.map_or(String::new(), |m| t(lang, m).to_string()), rect: (x, y, w, h), choices: Vec::new(), selected: 0 };
                match kind {
                    Label if words.is_none() => item.text = lines.next().unwrap_or_default(),
                    Secret => {
                        let _ = start.next();
                    }
                    Pick => {
                        item.selected = start.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                        item.choices = if picks == 0 {
                            vec![t(lang, Msg::PtAes256).to_string(), t(lang, Msg::PtAes128).to_string()]
                        } else {
                            vec![t(lang, Msg::PtPrintYes).to_string(), t(lang, Msg::PtPrintLow).to_string(), t(lang, Msg::PtPrintNo).to_string()]
                        };
                        picks += 1;
                    }
                    Check => item.selected = usize::from(start.next().as_deref() == Some("1")),
                    _ => {}
                }
                item
            })
            .collect::<Vec<FormItem>>();
        FormSpec { tag: TAG_PROTECT, title: t(lang, Msg::PtTitle).to_string(), size: (560, 414), items, ok: t(lang, Msg::PtSave).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None }
    }

    /// File > Passwords and permissions.
    pub(super) fn ask_protect(&mut self) -> Vec<Action> {
        let Some(info) = self.pt.info.clone().filter(|_| self.doc.is_some()) else { return Vec::new() };
        if self.pt.running.is_some() || self.pt.waiting.is_some() || self.print.is_some() || self.ocr.is_some() || self.edit.saving.is_some() || self.edit.exporting.is_some() {
            return self.say(t(self.lang, Msg::NoteBusy).to_string());
        }
        self.pt.last = None;
        vec![Action::AskForm(self.protect_form(&info))]
    }

    /// The box was closed: with the button the passwords are to come off, with OK the choices are checked and the person is asked where to
    /// write the file.
    pub(super) fn protect_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(FormAnswer { mut values, button }) = answer.filter(|a| a.values.len() == VALUES) else { return Vec::new() };
        let actions = self.protect_chosen(&values, button.is_some());
        // The passwords are in the engine's hands or nowhere: this copy is overwritten.
        values.iter_mut().take(4).for_each(wipe);
        actions
    }

    fn protect_chosen(&mut self, values: &[String], remove: bool) -> Vec<Action> {
        let lang = self.lang;
        let Some(info) = self.pt.info.clone().filter(|_| self.doc.is_some()) else { return Vec::new() };
        if info.signed || !info.may_change {
            return self.say(t(lang, if info.signed { Msg::PtSigned } else { Msg::PtOwnerNeeded }).to_string());
        }
        if remove {
            if info.cipher.is_some() {
                self.pt.forget_waiting();
                self.pt.waiting = Some((ProtectChange::Remove, String::new()));
                return self.pick_protect_path();
            }
            return Vec::new();
        }
        let v = |i: usize| values.get(i).map_or("", String::as_str);
        let (user, owner) = (v(0), v(2));
        let method = if v(4) == "1" { Method::AesV2 } else { Method::AesV3 };
        let (print, high) = match v(5) {
            "0" => (true, true),
            "1" => (true, false),
            _ => (false, false),
        };
        let on = |i: usize| v(i) == "1";
        let permissions = Permissions { print, print_high_quality: high, modify: on(7), copy: on(6), annotate: on(8), fill_forms: on(9), assemble: on(10), accessibility: on(11) };
        // What was chosen, but not the passwords, is kept for showing the box again after a mistake.
        self.pt.last = Some(values.iter().enumerate().map(|(i, s)| if i < 4 { String::new() } else { s.clone() }).collect());
        let mistake = if user != v(1) || owner != v(3) {
            Some(t(lang, Msg::PtMismatch).to_string())
        } else if !permissions.allows_everything() && owner.is_empty() {
            Some(t(lang, Msg::PtNeedOwner).to_string())
        } else if user.is_empty() && owner.is_empty() {
            Some(t(lang, Msg::PtNothing).to_string())
        } else {
            [user, owner].into_iter().find_map(|pw| match password_bytes(method, pw) {
                Ok(_) => None,
                Err(PasswordProblem::NotInPdfDocEncoding) => Some(t(lang, Msg::PtNotInDoc).to_string()),
                Err(PasswordProblem::NothingLeft) => Some(t(lang, Msg::PtBlank).to_string()),
                Err(PasswordProblem::TooLong { most }) => Some(t(lang, Msg::PtTooLong).replace("{n}", &most.to_string())),
            })
        };
        if let Some(text) = mistake {
            // The box comes back as it was, without the passwords.
            let again = self.protect_form(&info);
            return vec![Action::Message { title: t(lang, Msg::PtTitle).to_string(), text }, Action::AskForm(again)];
        }
        // The result is opened with the password that gives the most rights: the one who has just set the protection goes on working.
        let reopen = if owner.is_empty() { user.to_string() } else { owner.to_string() };
        let how = Protection { method, user_password: user.to_string(), owner_password: owner.to_string(), permissions, encrypt_metadata: true };
        self.pt.forget_waiting();
        self.pt.waiting = Some((ProtectChange::Set(Box::new(how)), reopen));
        self.pick_protect_path()
    }

    /// Ask where to write the file: the open one to begin with.
    fn pick_protect_path(&mut self) -> Vec<Action> {
        // The whole path of the open file: the box starts in its folder with its name, and asks before it replaces it.
        let name = self.path_text();
        let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())];
        vec![Action::PickSavePath { title: t(self.lang, Msg::SaveTitle).to_string(), filters, name }]
    }

    /// The box that asks for the file to write was answered. `None`: it was not asked for the protection.
    pub(super) fn protect_picked(&mut self, path: Option<PathBuf>) -> Option<Vec<Action>> {
        let (change, mut reopen) = self.pt.waiting.take()?;
        let Some(path) = path.map(|p| std::path::absolute(&p).unwrap_or(p)).filter(|_| self.doc.is_some()) else {
            wipe(&mut reopen);
            return Some(Vec::new());
        };
        let mut seed = [0u8; 32];
        if !(self.pt.random)(&mut seed) {
            wipe(&mut reopen);
            return Some(vec![Action::Message { title: t(self.lang, Msg::Error).to_string(), text: t(self.lang, Msg::PtNoRandom).to_string() }]);
        }
        let id = self.new_id();
        self.engine.protect(self.doc_id, id, &path.to_string_lossy(), change, seed);
        seed.fill(0);
        std::hint::black_box(&seed);
        self.pt.running = Some((id, path, reopen));
        Some(Vec::new())
    }

    /// The engine has written the file (or not). `None`: that request was not this one.
    pub(super) fn on_protected(&mut self, id: u64, status: SaveStatus, message: &str) -> Option<Vec<Action>> {
        if self.pt.running.as_ref().map(|r| r.0) != Some(id) {
            return None;
        }
        let (_, path, mut password) = self.pt.running.take()?;
        let lang = self.lang;
        let actions = match status {
            SaveStatus::Done => {
                // The file that was written is the document now, opened with the password the person set (or none).
                let mut actions = self.open_path_now(&path, &password);
                actions.extend(self.say(t(lang, Msg::PtDone).to_string()));
                actions
            }
            SaveStatus::Failed => vec![Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::PtFailed)) }],
        };
        wipe(&mut password);
        Some(actions)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::i18n::Lang;
    use qingpdf_core::Document;

    fn pump(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
        for _ in 0..1000 {
            app.event(Event::Wake);
            if until(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    /// Two pages of 300 x 300 points.
    fn two_pages() -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = std::collections::BTreeMap::new();
        let objects = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 300 300] >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R >>".to_string()),
            (4, "<< /Type /Page /Parent 2 0 R >>".to_string()),
        ];
        for (n, body) in &objects {
            offsets.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let at = out.len();
        out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for n in 1..5 {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        out
    }

    /// What the box is filled in with: the four passwords, the method (0: AES-256, 1: AES-128), printing (0: best, 1: low, 2: none), then
    /// the six rights (`1`: allowed) in the order copy, modify, annotate, forms, assemble, accessibility.
    fn answer(values: &[&str], button: Option<usize>) -> Event {
        Event::Form { tag: TAG_PROTECT, answer: Some(FormAnswer { values: values.iter().map(ToString::to_string).collect(), button }) }
    }

    const ALL_ON: [&str; 6] = ["1", "1", "1", "1", "1", "1"];

    fn values<'a>(passwords: [&'a str; 4], method: &'a str, print: &'a str, rights: [&'a str; 6]) -> Vec<&'a str> {
        passwords.into_iter().chain([method, print]).chain(rights).collect()
    }

    fn form_of(actions: &[Action]) -> &FormSpec {
        let [Action::AskForm(form)] = actions else { panic!("{actions:?}") };
        form
    }

    fn random(buffer: &mut [u8]) -> bool {
        for (i, b) in buffer.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(1);
        }
        true
    }

    fn no_random(_: &mut [u8]) -> bool {
        false
    }

    #[test]
    fn the_box_shows_the_protection_checks_the_passwords_and_writes_the_file_again_under_the_new_one() {
        let dir = std::env::temp_dir().join(format!("qingpdf-view-protect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("two.pdf");
        std::fs::write(&file, two_pages()).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.set_random(random);
        // Nothing open: nothing happens, and the menu item is grey.
        assert!(app.event(Event::Command(CMD_PROTECT)).is_empty());
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        let menu_item_on = |app: &App| app.menus().iter().any(|m| m.entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id, enabled: true, .. } if *id == CMD_PROTECT)));
        assert!(menu_item_on(&app));

        // The box of a file that is not protected: its state in words, four password boxes, two lists, six tick boxes (all ticked), no button
        // to take passwords off.
        let asked = app.event(Event::Command(CMD_PROTECT));
        let form = form_of(&asked);
        assert_eq!(form.tag, TAG_PROTECT);
        assert!(form.items.iter().any(|i| i.kind == FormKind::Label && i.text == "No password, nothing restricted."));
        assert_eq!(form.items.iter().filter(|i| i.kind == FormKind::Secret).count(), 4);
        assert!(form.items.iter().filter(|i| i.kind == FormKind::Secret).all(|i| i.text.is_empty()));
        let picks: Vec<&FormItem> = form.items.iter().filter(|i| i.kind == FormKind::Pick).collect();
        assert_eq!((picks.len(), picks[0].choices.len(), picks[1].choices.len(), picks[0].selected, picks[1].selected), (2, 2, 3, 0, 0));
        assert!(picks[0].choices[0].starts_with("AES-256") && picks[0].choices[1].starts_with("AES-128"));
        let checks: Vec<&FormItem> = form.items.iter().filter(|i| i.kind == FormKind::Check).collect();
        assert!(checks.len() == 6 && checks.iter().all(|c| c.selected == 1));
        assert!(form.items.iter().all(|i| i.kind != FormKind::Button), "nothing to take off");
        // Everything fits in the box (a list that drops down is as tall as its list).
        assert!(form.items.iter().all(|i| i.rect.0 >= 0 && i.rect.0 + i.rect.2 <= form.size.0 && (i.kind == FormKind::Pick || i.rect.1 + i.rect.3 <= form.size.1)));
        assert_eq!(form.items.iter().filter(|i| matches!(i.kind, FormKind::Secret | FormKind::Pick | FormKind::Check)).count(), VALUES);
        // Cancelled: nothing happens.
        assert!(app.event(Event::Form { tag: TAG_PROTECT, answer: None }).is_empty());

        // Mistakes are told in a message, and the box comes back with what was chosen but without the passwords.
        let mistake = |app: &mut App, v: Vec<&str>, word: &str| {
            let actions = app.event(answer(&v, None));
            let [Action::Message { text, .. }, Action::AskForm(again)] = &actions[..] else { panic!("{word}: {actions:?}") };
            assert!(text.contains(word), "{word}: {text}");
            assert!(again.items.iter().filter(|i| i.kind == FormKind::Secret).all(|i| i.text.is_empty()));
            again.items.iter().filter(|i| i.kind == FormKind::Pick).map(|i| i.selected).collect::<Vec<_>>()
        };
        mistake(&mut app, values(["a", "b", "", ""], "0", "0", ALL_ON), "not the same");
        mistake(&mut app, values(["", "", "o", "x"], "0", "0", ALL_ON), "not the same");
        mistake(&mut app, values(["u", "u", "", ""], "0", "0", ["0", "1", "1", "1", "1", "1"]), "permissions password");
        mistake(&mut app, values(["", "", "", ""], "0", "0", ALL_ON), "nothing to protect");
        assert_eq!(mistake(&mut app, values(["密码", "密码", "o", "o"], "1", "1", ALL_ON), "AES-256"), vec![1, 1], "the method and printing are kept");
        let long = "x".repeat(130);
        mistake(&mut app, values([&long, &long, "", ""], "0", "0", ALL_ON), "127");
        assert!(app.edit.asked.is_empty() && app.pt.waiting.is_none() && app.pt.running.is_none());

        // A good answer: AES-128, a user and an owner password, low quality printing, no copying. The box asks where to write the file,
        // with the open file's name to begin with; cancelling that box does nothing.
        let good = values(["user-pw", "user-pw", "owner-pw", "owner-pw"], "1", "1", ["0", "1", "1", "1", "1", "1"]);
        let asked = app.event(answer(&good, None));
        let [Action::PickSavePath { name, .. }] = &asked[..] else { panic!("{asked:?}") };
        assert!(std::path::Path::new(name).is_absolute() && name.ends_with("two.pdf"), "{name}");
        assert!(app.event(Event::SavePicked(None)).is_empty() && app.pt.running.is_none());
        // A system that gives no random numbers: nothing is written, and the person is told.
        app.set_random(no_random);
        app.event(answer(&good, None));
        let told = app.event(Event::SavePicked(Some(file.clone())));
        assert!(matches!(&told[..], [Action::Message { text, .. }] if text.contains("random")), "{told:?}");
        assert_eq!(std::fs::read(&file).unwrap(), two_pages(), "the file is as it was");
        app.set_random(random);
        // Written over the open file; then it is opened again, with the owner password (whoever has just set it goes on working).
        app.event(answer(&good, None));
        app.event(Event::SavePicked(Some(file.clone())));
        assert!(app.pt.running.is_some());
        pump(&mut app, "the file to be written and opened again", |a| a.pt.running.is_none() && a.has_document() && !a.opening && a.pt.info.as_ref().is_some_and(|i| i.cipher.is_some()));
        assert!(app.note.as_deref().is_some_and(|n| n.contains("opened again")), "{:?}", app.note);
        let locked = Document::open(&file).unwrap();
        assert!(locked.is_locked(), "the file needs the user password");
        let doc = Document::open_with_password(&file, "user-pw").unwrap();
        let enc = doc.encryption().unwrap();
        assert_eq!((enc.version, enc.revision), (4, 4));
        assert!(!enc.permissions.copy && enc.permissions.print && !enc.permissions.print_high_quality && enc.permissions.modify);
        assert!(Document::open_with_password(&file, "owner-pw").is_ok());
        // The window has the owner's rights; the box now says how the file is protected, and that it can be changed.
        assert!(app.rights.copy && app.rights.print_high_quality);
        let asked = app.event(Event::Command(CMD_PROTECT));
        let form = form_of(&asked);
        let words: Vec<&str> = form.items.iter().filter(|i| i.kind == FormKind::Label).map(|i| i.text.as_str()).collect();
        assert!(words.contains(&"Encrypted with AES-128."), "{words:?}");
        assert!(words.contains(&"A password is needed to open it."), "{words:?}");
        assert!(words.iter().any(|w| w.starts_with("Not allowed: ") && w.contains("Copy text and pictures")), "{words:?}");
        assert!(form.items.iter().any(|i| i.kind == FormKind::Button), "the button that takes the passwords off");
        // It starts as the file is: AES-128, low quality printing, copying off.
        let picks: Vec<usize> = form.items.iter().filter(|i| i.kind == FormKind::Pick).map(|i| i.selected).collect();
        assert_eq!(picks, vec![1, 1]);
        let ticks: Vec<usize> = form.items.iter().filter(|i| i.kind == FormKind::Check).map(|i| i.selected).collect();
        assert_eq!(ticks, vec![0, 1, 1, 1, 1, 1]);

        // The button takes the passwords off: where to write, then a file that is not encrypted, opened again.
        let asked = app.event(answer(&values(["", "", "", ""], "1", "1", ["0", "1", "1", "1", "1", "1"]), Some(0)));
        assert!(matches!(&asked[..], [Action::PickSavePath { .. }]), "{asked:?}");
        let plain = dir.join("plain.pdf");
        app.event(Event::SavePicked(Some(plain.clone())));
        pump(&mut app, "the passwords to be taken off", |a| a.pt.running.is_none() && a.has_document() && !a.opening && a.pt.info.as_ref().is_some_and(|i| i.cipher.is_none()));
        assert!(!Document::open(&plain).unwrap().is_encrypted());
        assert_eq!(app.path.as_deref(), Some(plain.as_path()));
        assert_eq!(Document::open_with_password(&file, "user-pw").unwrap().page_count().unwrap(), 2, "the protected file is still there");

        // The file opened with the user password only: the box shows the state and says what is needed; OK writes nothing.
        app.open_path(&file, "user-pw");
        pump(&mut app, "the file to open", |a| a.has_document() && !a.opening && a.pt.info.as_ref().is_some_and(|i| i.cipher.is_some()));
        assert!(app.pt.info.as_ref().is_some_and(|i| !i.may_change));
        let asked = app.event(Event::Command(CMD_PROTECT));
        let form = form_of(&asked);
        assert!(form.items.iter().any(|i| i.kind == FormKind::Label && i.text.contains("its owner password")));
        let after = app.event(answer(&values(["n", "n", "", ""], "0", "0", ALL_ON), None));
        assert!(after.iter().all(|a| !matches!(a, Action::PickSavePath { .. })) && app.pt.waiting.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn closing_the_tab_while_a_file_is_being_written_forgets_that_request_and_its_password() {
        let dir = std::env::temp_dir().join(format!("qingpdf-view-protect-reset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (first, second, target) = (dir.join("first.pdf"), dir.join("second.pdf"), dir.join("locked.pdf"));
        std::fs::write(&first, two_pages()).unwrap();
        std::fs::write(&second, two_pages()).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.set_random(random);
        app.open_path(&first, "");
        pump(&mut app, "the first file to open", |a| a.has_document());
        // A protection is asked for and sent to the engine; the answer has not come when another file is opened.
        app.event(Event::Command(CMD_PROTECT));
        app.event(answer(&values(["pw-one", "pw-one", "pw-two", "pw-two"], "0", "0", ALL_ON), None));
        app.event(Event::SavePicked(Some(target.clone())));
        assert!(app.pt.running.is_some(), "the request is on its way");
        // The tab is busy with it: another file is not opened beside it (a note says why), and the request goes on.
        app.open_path(&second, "");
        assert!(app.pt.running.is_some() && app.tabs.len() == 1 && app.note.as_deref().is_some_and(|n| n.contains("busy")), "{:?}", app.note);
        // Closing the tab: nothing of the request is kept, the password with it, and the box is not told "busy" for the rest of the session.
        app.event(Event::Command(CMD_CLOSE_TAB));
        assert!(app.pt.running.is_none() && app.pt.waiting.is_none() && app.tabs.is_empty());
        app.open_path(&second, "");
        pump(&mut app, "the second file to open", |a| a.has_document() && a.path.as_deref().is_some_and(|p| p.ends_with("second.pdf")));
        let asked = app.event(Event::Command(CMD_PROTECT));
        let form = form_of(&asked);
        assert_eq!(form.tag, TAG_PROTECT);
        // (The engine finishes what it was asked for, and its answer is for a document that is not open: the window goes on with the second.)
        app.event(Event::Form { tag: TAG_PROTECT, answer: None });
        for _ in 0..500 {
            app.event(Event::Wake);
            std::thread::sleep(Duration::from_millis(10));
            if target.is_file() {
                break;
            }
        }
        for _ in 0..20 {
            app.event(Event::Wake);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.path.as_deref().is_some_and(|p| p.ends_with("second.pdf")) && app.pt.running.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
