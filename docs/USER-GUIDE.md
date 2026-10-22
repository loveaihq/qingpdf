# qingpdf user guide

## 1. What it is

qingpdf is a small, fast, offline PDF tool. It needs no installation, does not go online, and collects no data.

It has two programs:

- **qingpdf-view.exe**: the reader. Use it to view PDFs. It can also add comments, fill in forms, edit text, and export to Word and Excel.
- **qingpdf.exe**: the command-line tool. A few commands merge, split, add watermarks and so on. It suits processing many files at once.

Both programs are only a few MB each. For now, only Windows is supported.

## 2. Download and open

1. Extract the zip file to a folder (right-click → Extract All).
2. Double-click **qingpdf-view.exe**.
3. Press Ctrl+O to choose a PDF file, or drag a PDF straight into the window. You can drag in several at once; each file gets its own tab.

The first time you run it, Windows may show "Windows protected your PC". This is because the program is not signed with a paid certificate, not because the program is faulty. Click "More info", then "Run anyway".

If the menus are in English and you want Chinese, click View → Language, choose 中文 (Chinese), and the change takes effect at once. The window size and position, the language, the command bar, the sidebar tab and width, the appearance, the accent colour and the page colours are remembered the next time you open the program.

## 3. Using the reader

The window is, from top to bottom: the **title bar**, the **command bar**, and the page area. There is no menu bar and no status bar.

- The **title bar** has the "轻" logo on the left and a tab for each file (a single file also gets a tab). The + to the right of the tabs opens a file. On the right are the Windows minimise, maximise and close buttons; on Windows 11, hovering over the maximise button shows Snap layouts. Drag the blank space beside the tabs to move the window, and double-click it to maximise or restore.
- **Command bar**, from left to right: Show sidebar, Open, Save and Print; then Select and pick, Highlight, Note, Draw, Shapes (click it for Rectangle, Ellipse, Line and Arrow), Text box and Mark an area (drag a box) (the redaction tool); then the rounded search box, and the "⋯" button on the far right. The tool in use and the sidebar button have a teal background. Buttons that cannot be used (no file is open, or the file does not allow comments) are greyed out. Hover over a button for a moment to see its name and shortcut.
- **Menus**: click "⋯", or press Alt or F10, to open the ten menus (File, Edit, View, Page, Comment, Insert, Redact, Tools, Go and Help). Each has submenus. Use ↑ ↓ ← → and Enter, or press the underlined letter in a menu name; press Esc to close. **Alt plus a letter** opens that menu directly (Alt+F for File, Alt+E for Edit, Alt+V for View, and so on; in the Chinese menus, use the letter in brackets). Inside a menu, pressing a letter chooses the item with that letter (if several items share the letter, press it again to reach the next one). The menus are drawn by the reader: each row is 32 pixels high; the selected row has a rounded teal background; shortcuts are on the right in a paler colour; ticks and dots (for choosing one of several) are teal; greyed-out items are pale; submenus have an arrow. Light and dark look the same. The right-click menus on the page and the thumbnails, and the lists opened from the Shapes button, look the same too. All menu items and shortcuts are unchanged.
- **Screen readers (Narrator, NVDA, JAWS).** When Windows says a screen reader is running (Settings > Accessibility > Narrator; it can be turned on or off while the program runs, and the window follows at once without restarting), the program uses the system's own parts, because a screen reader cannot read the names of the menus, buttons and lists the program draws itself. The window uses the system's title bar (the tab strip is below it). A **classic menu bar** (File, Edit, View, ...) appears under the title bar: Alt or F10 enters it, the left and right arrows move along it, Down opens a menu, and Alt with a letter opens one directly, as in other programs. All pop-up menus (the "⋯" menu, the right-click menus and the lists that drop down) are the system's plain text menus, with the system's ticks and dots. In dialogs the buttons and tick boxes are the system's buttons and the lists that drop down are the system's combo boxes (also the ones you can type in); Tab, Space, F4 and Alt+Down work as they do everywhere. The price is that the program does not look like itself then (no rounded corners or teal), and what you get is that a screen reader can read every control. Without a screen reader, the buttons, tick boxes and lists the program draws itself also answer the system's accessibility questions (their name, whether they are a button or a tick box, whether they are ticked, and which entry a list is on). **To see this look on a machine with no screen reader:** set the environment variable `QINGPDF_SCREEN_READER` to 1 before starting the program.
- **Sidebar** (F4) is a floating rounded card, 8 pixels away from the window edges and the command bar. At the top is a three-part selector: Bookmarks, Pages and Comments. Drag the gap on the right of the card to change its width. When it is shown or hidden, it slides in or out in 0.15 seconds; the pages are laid out at the new width at once, without waiting for the animation.
- **Motion is brief.** When the mouse moves onto a command bar button or a tab, its background fades in over 0.1 seconds and fades out when the mouse leaves. When you close a tab, the tabs after it slide into its place. The message strip fades in and out. If "Animation effects" is turned off in Windows (Settings > Accessibility > Visual effects), none of these movements happen and changes appear at once. Animations never slow down typing or page turning.
- **Page bar**: a rounded bar at the bottom centre of the page: ‹ page number / total › | − zoom percentage + | fit button. Click the page number or the zoom percentage to type a value (Enter confirms, Esc cancels). The fit button first fits the width; click it again to fit the whole page, and it switches back and forth. While you scroll, the bar fades; it comes back when you stop, so it does not block links on the page for long. There is no page bar in presentation mode.
- **Message strip**: a small dark strip above the page bar that says short messages ("Copied 12 characters", "Compressing... 41% (Esc cancels)"). There are two kinds. An **ordinary message** fades in, stays for about 5.5 seconds, then fades out. Only one shows at a time; later ones queue. A message that differs only by a number (such as progress) replaces the one on screen. A message that is too long is folded into two lines; only if it still does not fit is the middle of the second line left out, so the end of it ("Esc ends", "Ctrl+Enter") can always be read. Click it or press Esc to close it at once. A **message that stays** is shown for as long as it is true. While a job is running (printing, exporting, compressing, recognising text, looking for headings, writing pictures, looking for margins, redacting), the strip shows how far it has got ("Compressing... 41% (Esc cancels)") until the job is over. While a tool is in use (marking text, drawing, a note, a text box, changing text, a redaction box, a crop box, a signature picture), the strip shows that tool's hint until you press Esc to end the tool. An ordinary message goes first and the one that stays comes back after it. Click it to close it (it comes back only when it says something else). Esc does not close it: Esc is for the job and the tool. It stops printing, exporting, compressing and text recognition first, then closes an ordinary message, then the find box, then ends the tool. **A failure** (a change that was not made, a copy that did not work, a signature picture that could not be read, a file that changed on disk) is not put in the strip. It is a message box with the whole reason in it, which stays until you click OK.
- **Scroll bars**: thin bars at the right and bottom edges of the page (4 pixels wide, widening to 8 pixels when the mouse is over them). They disappear after 1 second without movement. You can drag them, click the empty part of the bar (to move one screen), or use the mouse wheel. Keyboard scrolling is unchanged.

### File

| Function | Shortcut | What it does |
|---|---|---|
| Open... | Ctrl+O | Choose a PDF file and open it in a new tab. If the file is already open, switch to its tab |
| Open recent | — | Lists the files you opened recently. "Clear the list" removes these records, and also the records of the page each file was last on |
| Close tab | Ctrl+W (Ctrl+F4) | Close the current tab. If it has unsaved changes, you are asked first |
| Save | Ctrl+S | Save your changes to this file |
| Save as... | Ctrl+Shift+S | Save as another file. The whole file is written again, so old text you changed does not stay in the new file |
| Export to Word... | — | Convert to an editable Word file (.docx) |
| Export to Excel... | — | Convert tables to an Excel file (.xlsx), one table per sheet |
| Export as images... | — | Save pages as PNG or JPEG pictures, one file per page (see "Exporting and extracting pictures" below) |
| Extract pictures from the pages... | — | Save the pictures on the pages as files, exactly as they are in the PDF (see the same section) |
| Document properties... | Ctrl+D | See the page count, size, protection, signatures, fonts and more, and change the title, author, subject and keywords (see "Document properties" below) |
| Passwords and permissions... | — | See how the file is encrypted now and what it restricts; set an open password and a permissions password, choose AES-256 or AES-128, turn off printing, copying and other permissions, or remove the passwords (see "Setting passwords and permissions" below) |
| Compress... | — | Make the file smaller and save it as a new file (see "Compressing a file" below) |
| Print... | Ctrl+P | Print the current file |
| Exit | — | Close the reader. If several tabs have unsaved changes, you are asked about each one |

**Several files (tabs):** each open file has a tab in the title bar. The selected tab is raised and has a teal line on top. Each tab shows the file name; unsaved changes are marked with a teal dot; the × on the right closes the tab. Hover over a tab to see the full path of the file. The + after the last tab opens a file.

- **Switching:** click a tab, or press Ctrl+Tab (next) or Ctrl+Shift+Tab (previous), or use the Go menu. Middle-click a tab to close it. The window title is always the current file.
- **Each tab keeps its own state:** the page you are on, the zoom, the rotation, the scroll position, which sidebar tab is open, the selected text, unsaved changes and the undo history. Comments, saving, exporting, printing, searching, recognising text and so on only affect the current tab.
- **Up to 16 files.** Opening more shows a message asking you to close a tab first.
- **How to open:** Ctrl+O, Open recent, dragging a file into the window, naming several files on the command line, or opening a file with qingpdf-view from File Explorer. If the reader is already open, opening a PDF with it (or starting it again from the command line) does not open a second window: the file is handed to the open window, shown in a new tab, the window comes to the front, and the new program exits at once. To turn this off, add the line `single_window=0` to the settings file (`%APPDATA%\qingpdf\author.txt`). If a dragged file is not a PDF or cannot be read, a message names the files (up to 8 names; the rest are only counted), and nothing else happens. Only the first 32 files of a drop are looked at, and device paths (`\\.\pipe\…`, `\\?\…`) are never looked at. A file on a network location that does not answer is given up after at most 2 seconds and treated as unreadable.
- **Busy tabs cannot be switched away from:** while a tab is saving, exporting, compressing, changing passwords, recognising text, printing, inserting pages or opening a file, switching shows "This tab is busy: wait for it to finish, or press Esc to stop it." (Press Esc to stop recognition, export and so on.) Clicking another tab while a file is opening waits until it has opened, and then switches. Closing a busy tab is fine; its jobs are cancelled with it.
- **Memory:** the total stays within 200 MB. The current tab gets the most. Other tabs keep only the file itself, unsaved changes, and a clear picture of the screen you last saw (up to 40 MB for all tabs together), so switching back to the few tabs you used most recently is instant. When there are too many tabs for the memory available, a tab with no unsaved changes and not opened with a password has its file released; when you click it again, the file is read from disk again (back to the same page and zoom, in tens to a hundred-odd milliseconds). If the file was changed in the meantime, you are told, and the current contents are shown; if the file has gone, you are told that too. Tabs with unsaved changes or opened with a password are never released. If there is no room for a new file, the message says "No memory left for another file: save or close some tabs first."
- **Going back to where you were:** when you open a file you looked at before, you return to the last page, zoom, scroll position and rotation (for the last 200 files; if another program has changed the file, so its size or modified time differs, it is treated as a new file and starts at page 1). The record is kept in `%APPDATA%\qingpdf\places.txt`. It holds only numbers and paths, with no passwords and no file contents. To turn this off, add the line `remember_place=0` to the settings file.

**Opening encrypted files:** if a file needs a password, the window asks for it ("This file needs a password:"), and a wrong password is asked for again. A file that only restricts printing or copying opens without a password. If the author does not allow copying, printing, comments, forms or changes, the related feature says that the owner password is needed. The owner password is the author's administration password; most people do not have it, so these features do not work for them.

**Setting passwords and permissions:** File → Passwords and permissions... The top of the window shows the file's current protection: the encryption method, whether a password is needed to open it, and which permissions are turned off. Below that you can set:

- **Password to open the file** (empty: none): the password needed to open the file. Type it twice; leave it empty for no password.
- **Permissions password** (empty: the same): the owner password that removes all the restrictions below. Type it twice; if left empty, it is the same as the open password. **If you turn off any permission, you must set a permissions password**, or anyone could remove the restriction in one click and it would protect nothing.
- **Encryption**: AES-256 (recommended; newer PDF readers all support it) or AES-128 (for very old software). An AES-128 password can only use Latin letters, up to 32 characters. For a password with Chinese or other non-Latin characters, use AES-256 (up to 127 bytes; a Chinese character takes 3 bytes).
- **Allow**: Printing (Yes, best quality / Low quality only / No), Copy text and pictures, Change the content, Add annotations, Fill in forms, Insert, delete and turn pages, Extract for screen readers.
- **Remove the passwords**: click the button at the bottom left of the window to save a copy with no passwords.

Click **Save...** and choose where to save: by default it is the file itself (Windows asks whether to overwrite), or you can save under a new name. Saving rewrites the whole file, and **unsaved changes are saved with it**. After saving, the file is opened again automatically, using the permissions password, so you keep full rights; other people are restricted when they open it. A file that is already encrypted and whose author has restricted it must first be opened with the owner password before you can change it. A file with a digital signature cannot be changed, because rewriting it would break the signature. **If you forget a password, it cannot be recovered**, so keep it safe.

Note: in the masked password boxes, Windows often does not let Chinese (or other non-Latin) input methods work. To type a Chinese password, copy it from somewhere else and paste it in.

**Compressing a file:** File → Compress..., choose a level, click **Compress...**, then choose where to save (by default, next to the file, with "-compressed" added after its name). Compression runs in the background; the message strip shows the progress, and **pressing Esc stops it**. When it finishes, it tells you the size before and after, and asks whether to open the new file. The original file is not changed.

- **What every level does (nothing is lost):** content that nobody uses is not written to the new file; repeated content (fonts, pictures, colour profiles and resources that each page carries) is stored once; objects are packed into object streams, and the cross-reference table becomes a compressed cross-reference stream (PDF 1.5; a file that says it is PDF/A-1 is not packed, and is written with the traditional cross-reference table, because that standard does not allow object streams, and compression says so); data streams are compressed again with the strongest Flate, and streams that were not compressed are compressed; page thumbnails, private data left by the program that made the file, and metadata on pages and pictures are removed (the metadata and document information for the whole file are kept).
- **Pictures (the difference between the three levels):** when a picture is drawn more finely than the level's dots per inch, it is reduced to that size and saved as JPEG, but only if the result is smaller. **High quality (200 dpi)**: JPEG quality 85. **Balanced (150 dpi, recommended)**: JPEG quality 75, the default. **Smallest (100 dpi)**: JPEG quality 60. How far a picture is reduced depends on the largest size it is drawn at on a page (the page, form and comment appearances are all checked). If you tick "Lossless only: do not change the pictures", only the first kind of change is made, and the pictures are left as they are.
- **Pictures that are left alone:** black-and-white pictures and masks (only recompressed); pictures with `/Decode` or colour-key masks, and CMYK, indexed-colour and 16-bit pictures; pictures in tiling patterns, Type 3 glyphs, and pictures drawn in a soft-mask group. A picture with a soft mask has its mask scaled to the same size.
- **Never gets bigger:** if the result is not smaller than the original, no new file is written, and you are told.
- **Encrypted files:** the new file keeps the original encryption (same method, same passwords). For a file whose author does not allow changing the content, making pictures smaller needs the owner password. Lossless-only compression (tick "Lossless only: do not change the pictures") does not need it, just as turning and deleting pages does not. A file with a digital signature cannot be compressed, because rewriting it would break the signature.
- **Unsaved changes:** before compressing, you are asked whether to save first; if you do not save, nothing is compressed. Files over 30 MB are not compressed in the reader, because they take too much memory. When the window is very large (for example on a 4K screen), files of about 16 MB or more are also refused, and you are told; use the command line for those.

### Edit

| Function | Shortcut | What it does |
|---|---|---|
| Undo | Ctrl+Z | Undo the last change. This also works after saving |
| Redo | Ctrl+Y | Do again a change you undid |
| Copy | Ctrl+C | Copy the selected text |
| Find... | Ctrl+F | Type what to find and press Enter to find the next match. Full-width and half-width characters, and upper and lower case, are treated as the same |
| Find next | F3 | Find the next match |
| Find previous | Shift+F3 | Find the previous match |

The command bar also has a search box. Type in it and press Enter to find; the number of matches, and which one you are on, are shown at the right end of the search box.

### View

| Function | Shortcut | What it does |
|---|---|---|
| Zoom in | Ctrl++ | Enlarge the page |
| Zoom out | Ctrl+- | Shrink the page |
| Fit width | Ctrl+1 | The page width fills the window exactly |
| Fit page | Ctrl+2 | The whole page fits in the window |
| Actual size | Ctrl+0 | Show the page at actual size |
| Rotate clockwise | Ctrl+R | Turn the page 90 degrees. This only changes the way you view it; the file is not changed |
| Show sidebar | F4 | Show or hide the sidebar beside the pages. The sidebar has three tabs at the top: "Bookmarks" (the table of contents); "Pages" (a small picture of each page, a fixed size that does not grow when you widen the sidebar; the current page has a teal rounded frame and a teal page number, and the thumbnail under the mouse has a pale background; click one to jump to that page; after you click, ↑ ↓ PgUp PgDn Home and End move from page to page, and the bar on the right can be dragged; click the page or the command bar to give the keyboard back to the page); and "Comments" (all the comments in this file; see "Comments list" below). If the file has no bookmarks, F4 opens Pages straight away. Drag the edge of the sidebar to change its width. The tab you last used is remembered the next time you open a file |
| Page layout | — | A submenu with one choice: "Single page, continuous" (one page after another, the usual way); "Two pages" (two pages side by side: 1 and 2, 3 and 4, and so on); or "Two pages, cover alone" (the first page on its own row, then 2 and 3, 4 and 5, and so on). Each file remembers its own setting, and it is still there the next time you open it. If you have not opened the file before, the file's own settings are used: a PDF that specifies two columns is shown as two pages; one that says its first page is on the right and alone is shown as cover alone; and for a vertical book set right to left, the two pages of a spread are swapped too. If the file says nothing, your last choice is used. In two-page view the two pages meet on the same vertical line, and they line up even when the pages are different sizes; the shorter page sits in the middle of its row. Zoom, find, selecting, comments, bookmarks, thumbnails (both pages have a blue frame), the page number box and Ctrl+G all work as usual. The Previous page and Next page buttons, and Ctrl+PgUp and Ctrl+PgDn, move one row (two pages) at a time; the page bar shows "Page 2-3 / 120" |
| Full screen | F11 | The window has no border, title bar, command bar or sidebar, and the page fills the monitor the window is on (the page bar, message strip and scroll bars are still shown). Press Esc or F11 again to return; the window goes back to how it was (size, position, maximised) |
| Presentation | F5 | One page at a time, fitted whole on a black background (each page is sized to its own dimensions, so scanned pages of different sizes work). Click, Space, PgDn, →, ↓ or Enter go to the next page; Backspace, PgUp, ←, ↑, Shift+Space or the right mouse button go back; the mouse wheel moves one page per notch; Home and End go to the first and last page. Links on the page can be clicked. The mouse pointer hides after 2 seconds without movement, and reappears when it moves. Press Esc to end; you stay on the page you reached, and the zoom goes back to what it was before. The next and previous pages are drawn in the background during the presentation, so page turns are instant |
| Appearance | — | A submenu: "Follow system" (reads the Windows app mode setting, and changes when Windows does), "Light", or "Dark". In dark mode the title bar, command bar, tabs, sidebar (Bookmarks, Pages and Comments), page bar, search box and the background around the pages are darker, and so are the Windows minimise, maximise and close buttons. On Windows 11 the title bar uses the Mica material, which lets the colour of the desktop wallpaper show through; on Windows 10 it is a single solid colour. **Unchanged:** the pop-up menus (the "⋯" menu, and the right-click menus on the page and the thumbnails) and the reader's own dialogs (the password box, the settings boxes) are drawn by the reader, so they follow Light and Dark too. Only the Windows dialogs (Open, Save, Print, folder selection, and the "Unsaved changes" message box) use the Windows look, because Windows gives programs no public way to change them. When Windows is set to a high-contrast theme, the window uses the system colours and the system's own title bar (the tab strip is below it) |
| Page colours | — | A submenu: "Original" (the file's own colours); "Night" (light and dark are swapped, but hues stay the same: white paper becomes near-black, black text becomes pale grey, yellow highlights stay yellow, and blue text stays blue); or "Eye-care (sepia)" (white paper becomes a warm yellow, #F4ECD8, and text stays black). **Only the screen is changed**: printing, exporting, copying text and saved files are not affected. Thumbnails change too. Selected text, found text, and the frames and handles of comments stay visible in every colour. Pictures change too (in Night, a photo looks like a negative); choose "Original" to see them as they are |
| Command bar | — | Show or hide the command bar (the title bar and tabs are not hidden). When it is hidden, Ctrl+F still works: the find box floats at the top right of the page, and the menus open with Alt or F10 |
| Language | — | Choose "Follow system", 中文 or "English". It takes effect at once |

The settings file (`%APPDATA%\qingpdf\author.txt`) also records a few more lines: `theme=` (`system`, `light` or `dark`), `page_color=` (`original`, `night` or `sepia`), and `layout=` (`single`, `spread` or `cover`, used for a file you have not opened before and which has no layout set). Each is changed by a single menu choice, and a line is not written until you change it from the default. Another line, `accent=`, takes `qing` (the default, teal) or `system` (the accent colour from Windows Settings; if it is too pale or too dark, it is adjusted until it can be seen). There is no menu item for it. To change it, add the line `accent=system`; to go back, delete the line. `toolbar=0` now means the command bar is hidden.

### Comment

Comments are marks laid over the page. They do not change the original text.

| Function | Shortcut | What it does |
|---|---|---|
| Select and pick | Esc | Return to normal mode, so you can select text |
| Highlight | — | Drag over the text to mark it, colouring it |
| Underline | — | Drag over text to underline it |
| Strikeout | — | Drag over text to strike it through |
| Squiggly | — | Drag over text to add a wavy underline |
| Draw | — | Hold the mouse button and drag to draw |
| Rectangle, Ellipse | — | Hold the mouse button and drag out a rectangle or ellipse. Hold Shift while dragging for a square or a circle. The outline uses Colour and Line width. Tick Fill shapes to fill it with a pale version of the same colour (the text underneath stays visible) |
| Line, Arrow | — | Hold the mouse button and drag from one end to the other. Hold Shift while dragging to snap the angle to multiples of 45°. The arrowhead is at the end where you release the mouse |
| Note | — | Click where the note should go, then type the note's text |
| Text box | — | Drag out a box and type text inside it. Click outside the box or press Ctrl+Enter to finish; Esc ends |
| Signature image... | — | Choose a PNG or JPEG picture (for example, a picture of your signature). Drag to move it, drag a corner to resize it, press Enter to place it, and Esc to cancel |
| Colour | — | Choose the colour of a comment: Yellow, Green, Blue, Pink, Red or Black. If a comment is selected, the change applies to it (this works only for highlight, underline, strikeout, squiggly, draw, rectangle, ellipse, line, note and text box comments; for other kinds these options are greyed out). If no comment is selected, the change applies to comments you add later |
| Line width | — | Choose Thin (1 pt), Medium (2 pt), Thick (4 pt) or Extra thick (8 pt). As above, if a Draw, Rectangle, Ellipse or Line comment is selected, the change applies to it |
| Fill shapes | — | Whether rectangles and ellipses are filled. If a rectangle or ellipse is selected, the change applies to it |
| Text size | — | The font size for text boxes: Small (10 pt), Medium (12 pt), Large (16 pt) or Huge (24 pt) |
| Author name... | — | Set the name shown on the comments you add |
| Remove annotation | Del | Delete the selected comment (Ctrl+Z undoes it). A comment its author has locked (the Locked flag in `/F`) cannot be deleted |

**Changing existing comments (ones we added, or ones added by other programs):**

| What to do | How |
|---|---|
| Select | With Select and pick active, click once. A selected comment has a blue frame, and the ones that can be resized have small squares at the four corners. The message strip says what it is, who wrote it and what it says. When you click on a link or a form field, the link or field takes priority. Highlights and other marks on text are different: dragging over them selects text, and a single click selects the comment |
| Move | Drag the selected comment (or drag a comment that is not on top of text). The arrow keys move it 1 point each time; hold Shift to move 10 points each time. A run of arrow key presses counts as one move, so one Ctrl+Z undoes it |
| Resize | Drag one of the small squares at the corners. Rectangles, ellipses, lines, drawings, text boxes, pictures and polygons can be resized; note icons and highlights on text cannot. Pictures always keep their shape; for the others, hold Shift to keep the shape. Shapes and text boxes are redrawn (line width and font size stay the same); pictures and similar comments are stretched |
| Change colour, line width or fill | After selecting it, use the Colour, Line width and Fill shapes menus |
| Change text | Double-click a note to change its text in the box that opens. Double-click a text box to change it in place (Ctrl+Enter or a click outside finishes; Esc cancels). Text that the author has locked (the LockedContents flag) cannot be changed |
| Locked comments | A comment its author has locked (the Locked flag) can be selected and have its text changed (the box of a text box stays the same; if the text does not fit, it is not changed), but it cannot be moved, resized, recoloured or deleted. A comment set to read-only cannot be changed at all. The message strip says "This annotation is locked: its author does not allow it to be changed or removed." |
| Comments from other programs that cannot be changed here | A comment that would lose something if it were redrawn is not redrawn; it can only be moved. These include dashed lines, cloud outlines, lines with leaders or caption text, other arrow styles, Acrobat-style text with rich formatting, text boxes with a border or background, notes with other icons, and semi-transparent text boxes. Text longer than 4,000 characters cannot be changed here. When you move a comment, the replies to it and its pop-up move with it |

All of these can be undone and redone with Ctrl+Z / Ctrl+Y, and saving writes them as one addition together with the other comments. In a file with a certification signature, only comments at certification level 3 may be changed; levels 1 and 2 are refused. If the author does not allow comments, the owner password is needed.

**Comments list (the Comments tab in the sidebar):** one heading per page ("Page 3"), with each comment on its own line below it. On the left is a small icon for the kind of comment. The first line is the comment's text (if it has none, its kind is shown); the second line is "author · last modified time" in a pale colour (if there is no author and time, the kind is shown). The selected row has a rounded teal background, and a row under the mouse has a pale background. Links, form fields, pop-ups and hidden items are not listed. Click a row to go to that page and select the comment; ↑ ↓ PgUp PgDn Home and End move through the list; Delete removes the selected comment (locked ones are not removed); the bar on the right can be dragged. If there are no comments, the list says "There are no annotations in this file." Thousands of comments are not slow. The list shows at most 20,000 comments, and at most 2,000 per page; if there are more, the message says "Only the first annotations are listed."

### Insert

These features write content into the page itself, so printing and other programs show it too.

| Function | What it does |
|---|---|
| Add Bookmark | Ctrl+B. Adds a bookmark at the top of the window, at this point (see "Editing bookmarks" below). The page content is not changed |
| Add text | Adds a piece of text to the page (Chinese text is supported) |
| Add picture... | Choose a PNG or JPEG picture and place it on the page |
| Add watermark... | Type the watermark text. You can set its opacity and angle. It can be tiled over the whole page, placed under the page content, or added to the current page only |
| Page numbers, headers and footers... | One window for headers and footers. Each has three places, left, middle and right, so six places in all. Click the drop-down arrow at a place to choose a common format (`{page}`, `第 {page} 页`, `第 {page} 页 共 {pages} 页`, `- {page} -`, `{page} / {pages}`, `Page {page} of {pages}`), or type your own text in any language. You can set the size, colour and distance from the page edge (in millimetres); the range of pages, from and to (leave empty for all pages); the starting number (for example, starting at 1 from page 3); mirrored odd and even pages (page numbers on the outer edge, as used when printing on both sides, as in official documents); and "Not on the first page". A small picture on the right shows how the current page will look as you type. "Remove those added before" removes only the page numbers and headers and footers this program added. You can undo the change; saving writes it as one addition together with other changes |
| Edit text on the page | Click a line of text, type the new text and press Enter (Esc cancels). The old letters are removed from the page |

**Fields in page numbers and headers and footers:** in the text, `{page}` is the page number of this page, `{pages}` is the number of the last page (numbered from 1 for the whole file, so it is the total number of pages), `{date}` is today's date (for example 2026-10-15; the reader uses the date on your computer), and `{file}` is the file name (without its folder). Other text is written out as it is.

**Page number notes:**

- Page numbers, headers and footers are written into the page, so printing and other programs show them. They are marked as the page's header and footer (not body text), so text extraction, find, copy, and export to Word and Excel do not include them.
- Page numbers count from the first page in the range. If the range is page 3 to the end and the starting number is 1, page 3 is numbered 1. If you choose "Not on the first page", the first page in the range is not numbered, but it still counts (so the next page is 2).
- "Odd and even pages mirrored" goes by the page's position in the file: pages 2, 4, 6 and so on are left pages, and on these pages the text at the left and right places is swapped; the middle is unchanged. To put numbers on the outer edge, put them on the right.
- Adding again adds another set. To replace them, first click "Remove those added before". That command only recognises what this program added: page numbers added by other programs cannot be removed, and if another program has rewritten the file, they may not be recognised (unrecognised ones are left alone). After removal, the font stays in the file (a few KB).
- Every character the text needs must exist in a system font. If one cannot be found, the reader says which one is missing. Files with a signature, and files whose author does not allow changing the content, cannot have page numbers added (the same as watermarks).

**Notes on editing text:** you can change one line at a time; a paragraph that runs over several lines must be changed line by line. If the new text is wider than the old, a warning says it may run into the text after it. After editing, the old text stays in the earlier version of the file. To remove it completely, use Save as.

### Redact

Redaction really removes the content you do not want people to see, rather than covering it with a black box. It takes two steps: first **mark** (a red frame appears on the page; the content is still there), then **Apply the marks...** (the content inside the red frames is deleted, and a black box is left in its place).

| Function | What it does |
|---|---|
| Mark an area (drag a box) | A tool: hold the mouse button and drag out a box; release to make one mark. Esc ends. It can cover text, pictures, or the text of a scanned page |
| Mark the selected text | Select text first, then choose this. Each line of text gets a red frame; a selection across pages gets one per page |
| Find and mark... | A window. Type the words to find (case and full-width and half-width characters are ignored), or tick ID card numbers, Mobile phone numbers or Bank card numbers. Every match on all pages is marked. Text in comments and form fields is also searched (the notes, field values and free text, together with the text drawn in their appearance), and a match there gets a mark over that comment. The message strip says how many places were marked; if none were found, nothing happens. If some pages could not be fully read (too large, font missing, content unreadable), a message box names the pages. Up to 5,000 places at once |
| Apply the marks... | A window that explains the consequences (see below). You can type "Words on the boxes" (for example "Redacted"; leave it empty for none), and tick "Also remove the document info and the metadata" and "Also remove attached files and scripts". Click **Apply...** and choose where to save: by default, a file named "name-redacted.pdf" next to the original. Choosing the original file overwrites it (Windows asks first). When it is done, the new file opens in a new tab (if you overwrote the original, its tab shows the new file). Anything you should know about (see "Things that stay in the file" below) is listed in a message box. The original file is not changed |

**A mark is a comment** (its kind is "Redaction mark"). It appears in the Comments list; you can select it, press Delete to remove it, and press Ctrl+Z to undo. It is saved with the other comments (other programs recognise this kind of comment too), so you can apply the marks later. The text and pictures inside a red frame stay visible, lightly tinted red.

**What applying the marks really removes:**

- **Text:** every character the box touches (any overlap between the character's cell and the box) is removed from the page content, including invisible text (the recognised text layer on scanned pages, render mode 3), text in form objects, watermarks and page numbers. Text outside the box stays where it is; text later on the same line does not move up to fill the gap. Standard fonts with no `/Widths` (Helvetica and the like) have guessed character widths, so the reader does not know where each character is. For such fonts, when a mark touches a line, **the whole line (every character in that operation) is removed**, not only the part inside the box. In marked content, `/ActualText`, `/Alt` and `/E` (which text readers and screen readers read from) are removed from **all** the marked page, whether they are written in the content, in the resource `/Properties`, or on structure-tree elements that point to that page. Text whose font is not in the page resources (or no font is selected) cannot be read, so it cannot be removed either: the whole page **fails**, and the page number is reported. A form object with no `/Font` resources of its own uses the fonts of the page that draws it (as PDFium does).
- **Pictures:** the pixels under the box are painted in the box's colour inside the picture itself, then re-encoded as they were (a high-quality JPEG stays a high-quality JPEG; others use Flate). The picture's transparency mask and soft mask are handled too. Small inline pictures on the page become picture objects. Where the same picture is drawn in several places, each place is changed separately, and the original picture is not touched. A picture that cannot be decoded is left out entirely.
- **Shapes:** lines, rectangles and other shapes lying completely inside the box are no longer drawn. **Shapes that are only partly in the box are left as they are** (the black box covers part of them, and they are not changed).
- **Comments and forms:** comments that overlap the box (links, notes, highlights and so on) and form fields are removed from the page. A note's pop-up and the replies to it are removed with it. A form field is removed together with its value from the form; when a form field is removed, the file's XFA data (which holds the form values) is removed too.
- **Patterns:** a pattern that nothing uses any more (every path that used it was deleted, and nothing else uses it; it may contain text) is removed from the resources.
- **Afterwards:** a black box is drawn in the place (in the mark's colour, with any words you typed), and the mark is removed. The page's thumbnail and the page's own metadata are discarded. A page whose content is split across several content streams is written as one stream; statements that run from one stream into the next are still removed. The whole file is **written again**, keeping only the objects that are still reachable, so old content is not in the new file. Encrypted files keep their encryption.

**These are not removed (they are reported, see below):** bookmark titles, the names of named destinations, attached files (and their descriptions), scripts (JavaScript), text on other pages in comments and form fields, text in structure-tree elements that do not point to a marked page, and document information and metadata (unless "Also remove the document info and the metadata" is ticked). **Also not removed (a font with a `/ToUnicode` table gets a note):** font files (they contain the glyph shapes, but not the order of the characters), and the font's `/ToUnicode` table (which maps character codes to characters; in a font where characters have been removed, it still lists them, and a name of two or three characters can be read back from it as **a set of characters, without their order**); Type 3 glyphs and pictures drawn in a soft-mask group; and shapes that are only partly inside the box (see above). If you are worried about these, first check the file another way, such as File → Compress... or Export to Word, or delete the whole page.

**Things that stay in the file are reported:** after applying, the program searches the new file for the places listed above, to see whether the removed text (runs of two or more removed characters, plus the words you searched for) is still there. Every place where it is found is reported: in the reader, in a message box; on the command line, as a `warning:` line. These are not page content, so redaction does not change them. You must edit or delete them yourself. A file with attachments or scripts always reports them, when present. Tick "Also remove attached files and scripts" (command line `--remove-attachments`) to delete them: the attachment and script name trees of the document catalogue, actions that run when the file opens, script actions on the document, pages and comments (including form controls), and linked files. Comments that had attachments become empty small comments. Actions on form fields themselves (not on the widget comments) are not handled.

**What rights you need:** a file whose author has not allowed "Change the content" (permission bit 4), and which is not opened with the owner password, cannot have marks applied. (Marking needs only the "Add annotations" right.) A file with a signature is refused, because rewriting the whole file would break the signature.

### Tools

| Function | What it does |
|---|---|
| Recognize text in all scanned pages | Scanned pages are pictures with no text. This uses the text recogniser that comes with Windows to read the words in the picture, so you can search and copy them afterwards |
| Recognize text on this page | Recognise only the current page |
| Bookmarks from Headings... | Creates bookmarks from the headings on the pages (by font size and weight), up to three levels. It first tells you how many headings it found and asks whether to create the bookmarks (if the file already has bookmarks, it says they will be replaced). Undo removes them |

Recognition is done on this computer; it does not go online or download anything. Pages that already have text are skipped. Recognised text can be removed with Undo.

**Chinese text needs the Chinese language pack for Windows.** Open Settings > Time & language > Language & region and add Chinese. For documents in another language, add the language pack for the language of your document in the same place. The reader does not change system settings.

### Go

| Function | Shortcut | What it does |
|---|---|---|
| Previous page | Ctrl+PgUp | Go to the previous page |
| Next page | Ctrl+PgDn | Go to the next page |
| Page number... | Ctrl+G | Type a page number to go to that page |
| Next tab / Previous tab | Ctrl+Tab / Ctrl+Shift+Tab | Switch between the open files (it wraps around) |

You can also click the page number in the page bar to type a page number, then press Enter to jump. (In two-page view, the page you type is shown in its row.)

### Help

| Function | What it does |
|---|---|
| About qingpdf... | Shows the version number and a short description |

### Managing pages (thumbnail bar)

On the Pages tab, select pages, then turn, delete, reorder, insert and extract them. All these actions can be undone and redone with Ctrl+Z / Ctrl+Y (they share one undo record with comments and text editing).

| What to do | How |
|---|---|
| Select pages | Click a page to select it. Ctrl+click adds a page to the selection or removes it; Shift+click selects a range. Press Ctrl+A in the thumbnail bar to select all. Selected pages have a blue background |
| Rotate left / Rotate right | Right-click → Rotate left or Rotate right, or press Ctrl+[ or Ctrl+]. This changes the page's own /Rotate, so other programs will also see the page turned after saving |
| Delete | Right-click → Delete, or press Delete in the thumbnail bar. You cannot delete every page. Deleting a page also removes its form fields. Bookmarks and links that point to a deleted page do nothing when clicked |
| Reorder | Hold the mouse button on the selected pages and drag them to a new position. An orange line shows where they will drop. Drag to the top or bottom edge of the bar to scroll automatically. Esc cancels |
| Insert pages from a file | Right-click → Insert pages from a file before... or after..., choose a PDF, and all its pages are inserted. Comments, form fields and links come with them; bookmarks do not. Fields with the same name are renamed automatically. Digital signatures in the other file are not brought in; their signature fields are kept but empty. A file cannot be larger than 16 MB, or larger than the memory left for unsaved changes. If it does not fit, you are asked to save the file first or to use a smaller file. A file that needs a password asks for it. A file that has any restrictions, and whose owner password you have not given, is refused, the same rule as merging on the command line |
| Extract to a new PDF | Right-click → Extract to a new PDF..., to write the selected pages as a new file. The open file is not touched, and encryption is kept as it was. Not possible if the author does not allow copying |

Saving (Ctrl+S) is an incremental save: the new page tree, turned pages and inserted pages are added after the original file, and the original bytes are left exactly as they are. So **deleted pages are still in the older part of the saved file**. To remove them for real, use Save as..., which writes the whole file again. When the author does not allow assembling pages (permission bit 11) and the file is not opened with the owner password, these commands are greyed out. A file with a signature cannot have its pages changed.

### Cropping pages (Page menu)

The Page menu has three items: Crop pages..., Crop by dragging a box, and Uncrop pages.... Cropping only changes which part of the page is shown: it changes the page's crop box (/CropBox). The content, comments and links on the page are left as they are, and the original page size (/MediaBox) is kept, so you can uncrop at any time. Saving (Ctrl+S) is an incremental save, and the original bytes are left exactly as they are, so **the cropped-away content is still in the file**. To remove it for real, use Redact.

| What to do | How |
|---|---|
| Crop by margins | Page → Crop pages...: how many millimetres to cut from the top, right, bottom and left, measured on the page as you see it (so the numbers are right even when the page is turned). "Apply to:" can be This page, The pages chosen in the thumbnails, All pages, The pages written below (type page numbers, such as 1-3,5), Odd pages, or Even pages |
| Cut the white margins automatically | In the same window, tick "Cut the white margins instead (the numbers are not used)". The program draws each page at a very low resolution and treats the most common colour on it as the paper (white, grey and cream scanned pages all work; colours within 20 levels count as paper). It finds the smallest area that is not paper and keeps 3 mm around it. Blank pages, and pages whose content fills the whole page (a full-page picture, with no white margin to cut), are left alone, and you are told which pages they are. If no page can be cut, the message says "There is no white margin to cut." The message strip shows the progress; press Esc to stop |
| Crop by dragging a box | Page → Crop by dragging a box. Drag on the page to outline the part to **keep**. When you release the mouse, the four margins in the window are filled in. Choose "Apply to:" and click **Crop**. Esc leaves this tool |
| Uncrop | Page → Uncrop pages...: select pages, and they go back to how they were before cropping. A file that already had a crop box (for example, one that hid crop marks outside it) gets its original crop box back, so nothing extra appears. A page that had no crop box shows the whole page again. Pages cropped by other programs also show the whole page again. Pages that were never cropped are not changed; if no page was cropped, you are told |

All these actions can be undone and redone with Ctrl+Z / Ctrl+Y (they share one undo record with comments and managing pages). If the author does not allow assembling pages (permission bit 11) or changing the content (permission bit 4), and the file is not opened with the owner password, these commands are greyed out. A file with a digital signature is refused (the same rule as for managing pages). After cropping, the page size changes, and the page and its thumbnail are redrawn at the new size at once.

### Exporting and extracting pictures

**File → Export as images...:** saves pages as pictures. In the window, choose: **Apply to** (This page, The pages chosen in the thumbnails, or All pages); **Format**: PNG (exact), or JPEG (smaller), with a quality from 1 to 100 (default 90); and **Resolution** (72, 150, 300 or 600 dpi, default 150). Click OK, then choose the folder for the pictures. Each page is one file, named `<name>-<page>.png` (or `.jpg`). The picture is the same as the page shows it, with comments included; the Night and Eye-care colours are not applied. If files with the same names are already in the folder, you are asked once, before any are replaced, how many there are and what the first one is called ("Replace them?"). Answering No does nothing. Progress is shown in the message strip, and pressing Esc stops it (the pictures already written are kept). Large pages are written in strips at high resolution, so they do not use much memory. Any page that could not be written (for example, because it is too large) is listed at the end.

**File → Extract pictures from the pages...:** saves the picture objects on the pages as files, exactly as they are in the PDF. A JPEG is not changed by a single byte (it is saved as .jpg), JPEG 2000 is saved as .jp2, and others are decoded and saved as PNG (with transparency, if the picture has a transparency mask). Names are `<name>-p<page>-<n>`. A picture used on several pages is saved only once. Pictures smaller than 2 points, pictures outside the page, and small pictures written directly into the page content (inline images) are not saved. **Existing files are never replaced**: if a file of that name is already in the folder, the new one gets -2, -3 and so on after its name. A JPEG with a transparency mask or colour inversion (`/Decode`), such as a logo with a transparent background, is decoded and saved as a PNG with transparency, not as the original opaque JPEG. If a picture to be decoded is larger than 40 million pixels, the reader does not save it and says so; use the command line `qingpdf images` for it.

Both commands are greyed out when the author does not allow copying (permission bit 5) and the file is not opened with the owner password. While exporting or extracting, the tab cannot insert or delete pages until the job is finished.

### Document properties

**File → Document properties...** (Ctrl+D) opens a window showing: the file name, folder and size; the PDF version, the number of pages, the size of the current page (in millimetres and inches), the program that created it, and its producer; the Title, Author, Subject and Keywords; the Created and Modified times (in your computer's time zone); how the file is encrypted (method, what is allowed and what is not); whether it is signed, tagged (accessible) and linearised (set up for fast opening on the web); the number of attachments; whether it has JavaScript; whether it has XMP; and the fonts used (name, kind, and embedded, subset or not embedded). The list of fonts has a limit: if there are very many, it stops partway and says so. The window reflects unsaved changes, so if you changed the title and have not saved, the new title is shown here.

The Title, Author, Subject and Keywords lines can be changed; click **OK** to save. This changes the document information. If the file has XMP metadata, the matching items in the XMP (dc:title, dc:creator, dc:description and pdf:Keywords) are changed together, so the two agree. If the XMP cannot be changed, it is left as it is, and you are told. The Created and Modified times are not changed. Clearing a line removes that item. This is one change that can be undone, and when you save (Ctrl+S) it is added to the end of the file. It needs the right to change the content. If the author does not allow changes (and the file is not opened with the owner password), or the file has a digital signature, the four lines can only be read, and the window says why.

### Editing bookmarks (Bookmarks tab in the sidebar)

Bookmarks (the table of contents) can be edited directly in the sidebar. All these actions can be undone and redone with Ctrl+Z / Ctrl+Y (they share one undo record with comments, text editing and managing pages). When you save (Ctrl+S), only the bookmark objects that changed are added, and the original bytes are left exactly as they are. Right-click a bookmark (or an empty area) to get a menu with the items below.

| What to do | How |
|---|---|
| Add Bookmark | Ctrl+B (also works after clicking in the sidebar), or right-click → Add Bookmark. It is added at the top of the window, at this point: after the selected bookmark (together with the ones under it), at the same level. If nothing is selected, it is added at the end. **Bookmark name:** if some text is selected, that text is used. Otherwise the nearest heading on this page is used (found the same way as for Export to Word). If the page has no heading, it is called "Page N". In a file with no bookmarks, the Bookmarks tab opens automatically in the sidebar after you add the first one |
| Rename | F2, or right-click → Rename. You can also click a selected bookmark, wait a moment, and click it again. Edit it in place; input methods, including Chinese ones, work as usual. Press Enter to confirm, Esc to cancel. A name can be up to 300 characters, and an empty name is not accepted. A bookmark with no name shows "(untitled)" in the tree. Pressing Enter in its box without changing the text does not count as a change |
| Delete | Delete, or right-click → Delete. This also deletes the bookmarks under it. It can be undone |
| Drag to reorder or change level | Hold the mouse button on a bookmark and drag. Dropping on the top third of a row places the bookmark before that row (a line shows where). Dropping on the bottom third places it after (a line shows where; if that row is expanded, it goes before its first child). Dropping in the middle places it inside that row (the row turns teal). The bookmarks under it move with it. Drag to the top or bottom edge of the sidebar to scroll automatically. Esc cancels. You cannot drag a bookmark into itself, and you cannot go deeper than 32 levels |
| Make a Child of the One Above / Move Up a Level | Tab / Shift+Tab, or right-click → Make a Child of the One Above or Move Up a Level. Making a child puts the bookmark under the one before it, as its child. Moving up a level raises it one level and leaves its position unchanged (bookmarks that came after it at the same level become its children). The first bookmark cannot be made a child, and the top level cannot be moved up |
| Set to the Current Position | Right-click → Set to the Current Position. This bookmark will from now on jump to the top of the window at this position (`/XYZ left top null`; the zoom is not changed). A named destination is replaced with an explicit position |
| Expand, collapse | Click the plus or minus sign in front of a bookmark. The expanded or collapsed state is saved as the sign of `/Count`, and is written the next time you change the bookmarks. Expanding or collapsing alone does not mark the file as modified |
| Bookmarks from Headings... | Tools → Bookmarks from Headings... (also in the right-click menu). A 468-page book takes under 1 second |

**Other bookmark actions are left alone:** if a bookmark is a link (a web address), a named action (such as "next page"), a jump to another file, or a script, its action is kept when you change its name or move it. "Set to the Current Position" is greyed out for it; to change its position, do so in another program. Named destinations (`/Names`) are never touched.

**Permissions:** if the author does not allow assembling pages (permission bit 11) or changing the content (permission bit 4), and the file is not opened with the owner password, all the bookmark commands are greyed out. The top line of the right-click menu says why, and Ctrl+B also shows a message in the message strip. A file with a digital signature is always refused (the same rule as for managing pages; the message says "signed"). An encrypted file stays encrypted after changes, and bookmark titles are encrypted together with the other strings.

**How they are written:** a title is stored in PDFDocEncoding if possible; otherwise it is stored as UTF-16BE with a byte order mark. Each bookmark has `/Parent`, `/Prev`, `/Next`, `/First`, `/Last` and `/Count` (for an open bookmark, the number of child bookmarks; for a closed one, a negative number). Other programs (PDFium, PyMuPDF and qpdf) read the same titles, levels and page numbers. **Limits:** at most 20,000 bookmarks, 32 levels, and 300 characters per title. For a file over these limits, the sidebar shows only the first ones it could read, and changing them is refused (otherwise the ones not read would be lost). Bookmarks from Headings, and the command-line `--import` and `--auto`, replace the whole set, and can still be used. **Save as**, or the first change to a damaged file, writes the whole file again. The bookmark object numbers change with it, and the sidebar reads the bookmarks again by itself, so further changes are not affected.

### Working on the page directly

- Drag the mouse to select text. Double-click selects a word (in Chinese, a whole run of text up to the punctuation), and triple-click selects a line.
- Right-click on the page to get Copy, Highlight selection, Add note here, or Select all on this page.
- **Forms:** click into a form field on the page to fill it in. If you make a mistake, press Ctrl+Z to undo it.

## 4. Command-line tool

**How to open a command prompt:** in the folder that holds qingpdf.exe, click the address bar at the top of the window, type `cmd` and press Enter. The black window that opens is the command prompt. Type the commands below into it and press Enter to run them.

Notes:

- If a file name contains spaces, wrap it in double quotes, for example `"合同 终稿.pdf"`.
- An existing output file is never overwritten. To overwrite it, add `--force` to the end of the command.
- Page ranges: `1-3,5` means pages 1, 2, 3 and 5; `8-` means page 8 to the last page.
- For an encrypted file, add `--password <password>`.

**Info**: shows the number of pages, the page sizes (in points and millimetres), the file information, and whether the file is encrypted. It also shows whether the file is signed, tagged or linearised, the number of attachments, whether it has JavaScript and XMP, and the creation and modification times. Add `--fonts` to also list the fonts used (name, kind, and whether embedded).

```
qingpdf info 合同.pdf
```

**Merge**: joins several PDFs one after another, in the order given.

```
qingpdf merge 合同.pdf 附件.pdf -o 合并.pdf
```

**Split**: `--pages` takes out the pages you name. `--every` cuts the file into one file every N pages; the output name must contain `%d`, which becomes 1, 2, 3 and so on.

```
qingpdf split 合同.pdf --pages 1-3,5 -o 前三页和第五页.pdf
qingpdf split 合同.pdf --every 10 -o 第%d份.pdf
```

**Delete pages**: deletes the pages you name.

```
qingpdf delete 合同.pdf --pages 2,4-6 -o 删页后.pdf
```

**Rotate**: turns the pages you name 90 degrees clockwise; `--angle -90` turns them anticlockwise. Without `--pages`, all pages are turned.

```
qingpdf rotate 扫描件.pdf --pages 1-3 --angle 90 -o 转正.pdf
```

**Remove encryption**: writes a copy with no password. This only works if you know the owner password, or if the file allows everything. It does not work on a signed file, because rewriting the whole file would break the signature.

```
qingpdf decrypt 加密合同.pdf -o 无密码合同.pdf --password 密码
```

**Encrypt and set permissions**: writes a copy with a password, with some permissions turned off. It uses AES-256 by default (`--aes128` gives AES-128). `--user-password` is the open password, and `--owner-password` is the permissions password. If you give only an open password, the permissions password is the same as it. The passwords can also be put in a text file and read with `--password-file`: the first line is the open password, the second line is the permissions password, and either line may be empty. This way, people cannot see what you typed in the command. A password must not have spaces before or after it, and must not contain control characters. (In cmd, `echo 密码 > 文件` adds a space after the password, and it is refused. Write the file in Notepad and save it as UTF-8.) All permissions are allowed by default. Use these options to turn them off: `--print none|low|high` (no printing, low quality only, or high quality), `--no-copy` (copying text and pictures), `--no-modify` (changing the content), `--no-annotate` (comments), `--no-forms` (filling in forms), `--no-assemble` (inserting, deleting and turning pages), and `--no-accessibility` (extraction for screen readers). `--cleartext-metadata` leaves the metadata unencrypted, so search programs can read it. If any permission is turned off, a permissions password is required. If no password is given and no permission is turned off, the command is refused, because there is nothing to protect. If the input file is already encrypted, give its password with `--password`, and you must have owner rights (the same rule as `decrypt`). A signed file is refused. The output is a new file written in full, and it cannot have the same name as the input. Passwords never appear in any output.

```
qingpdf encrypt 合同.pdf -o 加密合同.pdf --user-password 打开用的密码 --owner-password 管理用的密码
qingpdf encrypt 报告.pdf -o 只读报告.pdf --owner-password 管理用的密码 --no-copy --no-modify --print low
qingpdf encrypt 合同.pdf -o 兼容旧软件.pdf --aes128 --password-file 密码.txt
```

To remove the passwords, use `decrypt` above. To change a password, run `encrypt` again on the encrypted file, giving the old password with `--password`.

**Compress**: writes a smaller copy, and prints the size in bytes before and after. `--preset high|balanced|small` chooses a level. The default is `balanced`: pictures drawn at more than 150 dpi are reduced to 150 dpi, with JPEG quality 75. `high` is 200 dpi with quality 85; `small` is 100 dpi with quality 60. `--lossless` does only the parts that lose nothing (pictures are left alone). Every run removes content nobody uses, merges duplicate objects, packs objects into object streams and compressed cross-reference streams, recompresses data streams at the strongest setting, and removes thumbnails and private data. For an encrypted file, give the password with `--password`; the new file keeps the same encryption. For a file whose author does not allow changing the content, making pictures smaller needs the owner password; `--lossless` does not. A signed file is refused. **The output is never bigger than the input.** If the result is not smaller, the output is a copy of the input, and the command says so.

```
qingpdf compress 扫描件.pdf -o 小扫描件.pdf
qingpdf compress 画册.pdf -o 画册小.pdf --preset small
qingpdf compress 合同.pdf -o 合同小.pdf --lossless
```

**Extract text**: outputs the text in reading order. Without `-o`, it is shown in the window. A scanned file has no text, so recognise it in the reader first.

```
qingpdf text 合同.pdf -o 合同.txt
```

**Render pages as pictures**: saves each page as a picture, PNG by default. A larger `--dpi` is sharper. The default is 150 and the maximum is 2400 (one page may have at most 250 million pixels). `--format jpeg --quality 90` saves JPEG (quality 1 to 100, default 90). Large pages are written in strips, so high resolutions do not use much memory.

```
qingpdf render 合同.pdf --pages 1-3 --dpi 150 -o 第%d页.png
```

**Extract pictures**: saves the pictures on the pages to a folder (created if it does not exist), exactly as they are in the PDF. A JPEG is not changed by a single byte (`.jpg`), JPEG 2000 is saved as `.jp2`, and others (including JPEGs with a transparency mask or `/Decode`) are decoded and saved as PNG. First, every file name is checked: if a file of the same name exists and `--force` is not given, nothing is written. Names are `<name>-p<page>-<n>`, and a picture used on several pages is saved once. A file whose author does not allow copying needs the owner password.

```
qingpdf images 画册.pdf --pages 1-5 -o 图片
```

**Crop**: hides parts of the page from view by setting the page's crop box. The page content is not changed, and the hidden parts stay in the file. `--margins 上,右,下,左` gives how many millimetres to cut from each side, measured on the page as shown. `--auto` cuts the white margins automatically (the paper is the most common colour on the page, white or grey), keeping 3 mm around the content. A page whose content fills the whole page is not cropped, and a warning is given. The output is a new PDF, with the change added after the original file.

```
qingpdf crop 扫描件.pdf --margins 10,15,20,25 --pages 1-3 -o 裁后.pdf
qingpdf crop 公文.pdf --auto -o 去白边.pdf
```

**Change properties**: changes the title, author, subject and keywords (the document information and the XMP are changed together). Items you do not give are left alone, and so are the creation and modification times. Giving an empty value removes that item. The output is a new PDF, with the change added after the original file.

```
qingpdf meta 报告.pdf --title "年度报告" --author 办公室 --keywords "年度, 报告" -o 改属性后.pdf
```

**Images to PDF**: one page per image, in the order given. By default the pages are A4. Add `--page fit` to make each page the same size as its image.

```
qingpdf img2pdf 第一张.jpg 第二张.png -o 图片合集.pdf
```

**Watermark**: adds a text watermark. `--tile` covers the whole page, `--angle` sets the angle, `--opacity` sets the transparency (0 to 1), `--under` puts the watermark under the page content, and `--pages` chooses the pages. You can also add a picture watermark with `--image 水印.png`.

```
qingpdf watermark 合同.pdf --text "机密" --opacity 0.3 --angle 45 --tile -o 加水印.pdf
```

**Page numbers, headers and footers**: adds page numbers, headers and footers. There are three places each for the header and the footer, six in all: `--header-left`, `--header-center`, `--header-right`, `--footer-left`, `--footer-center` and `--footer-right`. If none is given, the middle of the footer gets `{page}`. In the text, `{page}` (the page number), `{pages}` (the number of the last page), `{date}` (today's date; the command line uses UTC, so it can differ from your computer's date by a day, while the reader uses your computer's date) and `{file}` (the file name) are replaced with the real values. `--size` sets the font size (default 10.5), `--color` the colour (six hex digits, default 000000), and `--margin` the distance from the page edge in millimetres (default 15). `--pages` chooses the pages; numbering counts from the first page in the range. `--start` sets the number of the first page (default 1). `--mirror` swaps left and right on pages 2, 4, 6 and so on, so the numbers are on the outer edge when printing on both sides. `--skip-first` leaves out the first page in the range, but it still counts. The output is the original file with a block of content added after it; the original bytes are kept exactly. Running it again adds another set.

```
qingpdf pagenum 报告.pdf --footer-center "第 {page} 页 共 {pages} 页" -o 带页码.pdf
qingpdf pagenum 公文.pdf --footer-right "- {page} -" --mirror --size 14 --margin 25 -o 公文带页码.pdf
qingpdf pagenum 论文.pdf --header-right "{file}" --footer-center "{page}" --pages 3- --start 1 -o 论文带页码.pdf
```

**Stamp a picture**: places a picture, such as a signature or logo, on the page you name. `--at` is the position of the picture's lower-left corner, and `--width` is its width. Both are in points (1 inch = 72 points).

```
qingpdf stamp 合同.pdf --image 签名.png --page 3 --at 72,100 --width 120 -o 盖章后.pdf
```

**Add text**: adds a piece of text at a position on the page you name. `--at` is the position of the lower-left corner of the first line, and `--size` is the font size in points. Write `\n` in the text to start a new line.

```
qingpdf addtext 合同.pdf --page 1 --at 72,720 --size 12 --text "你好" -o 加字后.pdf
```

**Replace text**: changes "旧词" (old word) to "新词" (new word) on the pages. It works one line at a time and does not cross lines. The old text stays in the earlier content of the file. To remove it completely, rewrite the file with `qingpdf split 改字后.pdf --pages 1- -o 干净版.pdf`.

```
qingpdf replace 合同.pdf --find "旧词" --replace "新词" --pages 1-3 -o 改字后.pdf
```

**Redact**: really removes content at certain places on the pages, and draws a black box in its place. There are three ways to give places, and each can be given several times: `--find 文字` (every match on the page; case and full-width and half-width characters are ignored); `--pattern idcard|phone|bankcard` (ID card numbers: 18 digits, the last of which may be X; mobile phone numbers: 11 digits starting with 13 to 19; bank card numbers: 16 to 19 digits, which may be grouped with spaces or hyphens; a number inside a longer string of digits is not matched); and `--area 页码:X,Y,宽,高` (the page number counted from 1, then X and Y as the lower-left corner of the box in points, measured from the lower-left corner of the page, plus the width and height, going right and up). Marks already made in the reader are also processed. `--color 000000` is the colour of the box, `--label 文字` is the text on the box (for example `已涂黑`), `--clear-metadata` also removes the document information and metadata, and `--remove-attachments` also removes attachments and scripts. The command prints how much text, how many pictures, shapes and comments it removed on each page. A `--find` or `--pattern` that finds nothing gives a warning. **If no place at all is found, the exit code is 1 and no file is written.** The output is a new file written in full (the old content is not in it), and it cannot have the same name as the input. For an encrypted file, use `--password` (you must have the right to change the content, or the owner password), and the new file stays encrypted. A signed file is refused. If something on a page cannot be read (an unknown filter), or text cannot be read (its font is not in the page resources), the whole command fails and reports the page number, and no half-finished file is handed over. Attachments, scripts, and places where removed text may still remain (bookmarks, names, metadata and so on) are reported in `warning:` lines.

```
qingpdf redact 合同.pdf --find "张三" --pattern idcard --pattern phone --area 1:72,600,200,40 --label 已涂黑 -o 合同-涂黑.pdf
```

**Convert to Word**: converts the pages to an editable Word file.

```
qingpdf docx 报告.pdf --pages 1-10 -o 报告.docx
```

**Convert to Excel**: converts the tables on the pages to an Excel file, one table per sheet.

```
qingpdf xlsx 对账单.pdf --pages 1-10 -o 对账单.xlsx
```

**Bookmarks**: `--export` writes the bookmarks to a text file; `--import` replaces the bookmarks with those in a text file; `--auto` creates bookmarks from the headings on the pages (up to three levels). The last two write the original file with the changes added after it, so the original bytes are kept exactly.

```
qingpdf bookmarks 报告.pdf --export 目录.txt
qingpdf bookmarks 报告.pdf --import 目录.txt -o 新目录.pdf
qingpdf bookmarks 报告.pdf --auto -o 带目录.pdf
```

A text file has one bookmark per line. Each level is one tab at the start of the line, then the title, one tab, and then the page number (counted from 1; leave it empty if the bookmark does not point to a page). For example, Chapter 1 on page 3 and Section 1 on page 4:

```
第一章	3
	第一节	4
```

Reading the file accepts a byte order mark or no BOM, and CRLF or LF line endings. Blank lines are skipped. **Levels are recognised only by tabs** (one per level). Spaces around the title and page number are removed. Problem lines are reported with their line number: a missing page number; a page number not in the file; an empty title; a title that starts with a space (usually a level written with spaces); a tab inside a title (a stray tab at the end of a line would join the page number to the title, so it is refused); a title over 300 characters; a level more than one deeper than the line before it (or more than 32 levels); or more than 20,000 bookmarks. A bookmark with no title is written as `(untitled)` on export, and on import that is its title. If a file has more than 20,000 bookmarks, or nests deeper than 32 levels, `--export` writes only the first ones it can read. `--import` and `--auto` replace the whole set anyway. All three commands print a warning line to standard error. A text file can be at most 4 MB. Bookmarks made by `--import` all jump to the top of their page, and all the original bookmarks (including link-type ones) are replaced. Encrypted files whose author does not allow assembling and changing the content (permission bits 11 and 4), and which are not given the owner password, are refused, and so are signed files.

## 5. Known limitations

1. **Windows only.** The reader and the command-line tool only run on Windows.
2. **Only horizontal text is recognised.** Text recognition handles horizontal text only. Scans of vertical old books come out as horizontal rows across the columns, and the reading order will be wrong.
3. **Tables with no lines may not be found correctly.** When converting to Excel and Word, tables without lines are found by aligning the text. Two-column tables such as "Name | Description", and tables with very narrow gaps between columns, may not be recognised. Some pictures may also be taken for tables.
4. **Text editing is limited.** You can change one line at a time; a paragraph that runs over several lines cannot be changed as one. Some text in forms that are used repeatedly cannot be changed either. The old text stays in the old version of the file. To remove it completely, use Save as.
5. **Signed files cannot have their content changed.** Adding watermarks, page numbers, pictures or text, editing text, and turning, deleting, reordering or inserting pages are all refused. A "signature image" only places a picture; it is not a digital signature. This program cannot sign documents or verify signatures.
6. **Word and Excel output is not an exact copy.** The Word file is an editable layout, not an exact copy of the page. Charts and drawn shapes are not converted.
7. **Only AES-256 and AES-128 are written for encryption.** RC4 is not written (it is too weak), and certificate (public-key) encryption is not supported. RC4 in old files that are opened can still be read. The AES-128 file key is computed from the password, the permissions and the file ID, as the standard requires, so it is not random (the standard defines it that way); the initialisation vector of each string and stream is still random. Encryption writes a new file; it does not add to the original file.
8. **Several files have limits.** Up to 16 files can be open at once. A busy tab (saving, exporting, compressing, recognising, printing and so on) cannot be switched away from. If many large files are open and memory runs short, unchanged tabs have their file released, and clicking them again reads it again. That is slower than switching to a recently used tab: for a 100-page file, tens to a hundred-odd milliseconds, and longer for larger files. The path tooltip on the tab strip shows only while the mouse is over it. If the reader is already open, starting it again from the command line hands the file to that window, so no new window appears (set `single_window=0` to turn this off).
9. **Viewing limits.** Two-page view is continuous only: pages flow row by row as you scroll. There is no layout that shows one pair of pages at a time without scrolling (the TwoPageLeft and TwoPageRight layouts). If a file asks for these, it is treated as "Two pages" or "Two pages, cover alone". Right-to-left reading is used only if the file asks for it; there is no separate switch in the menu. Presentation has no laser pointer, black screen, page jumping or automatic page turning. When scanned pages differ in size, each page is fitted to the screen at its own size, so neighbouring pages may have slightly different zoom. Dark mode covers the interface and the area around the pages; the menus and the reader's own dialogs are dark too. Windows' own dialogs (Open, Save, Print and the message boxes) stay in the Windows look. In Night mode, picture colours are inverted. On a 4K window, two-page view draws both pages at once, so turning pages is slower than in single-page view. The slowest case, when nothing has been drawn yet, takes 0.3 to 0.5 seconds per row. Normally, when the next row is already drawn, it takes a few to a few tens of milliseconds.
10. **Redaction limits.** Shapes that are only partly in the box are left unchanged (the black box covers them, and they are not changed). Type 3 glyphs and pictures drawn in soft-mask groups are not processed. Font files are not changed. Bookmark titles, named destinations, attachments, scripts, comments on other pages, structure-tree elements that do not point to marked pages, and the document information and metadata (unless "clear" is ticked) are not changed; they are only reported. Attachments and scripts can be removed too. A comment's `/RC` is not read when it is a stream. If a pattern is used as a colour, and another colour is then chosen without a `Q` in between, that pattern is treated as still in use and is not removed. The actions (scripts) of form fields themselves are not counted or removed. When the same form is drawn under two names and contains things to remove, the whole redaction fails and the page number is reported. Text whose font is not in the page resources cannot be read, and that whole page fails. Standard fonts without `/Widths` (Helvetica and so on) have guessed character widths, so the whole line that a mark touches is removed, and a note is shown; check that page after redacting. The `/ToUnicode` table of a font is not cut (see above). A picture rotated by an angle other than a multiple of 90 degrees has every pixel in the box's surrounding rectangle painted over (larger than the box, never smaller). At most 20,000 boxes per page. In the reader, redaction only works on files up to 40 MB; use the command line for larger files. It cannot be cancelled while it is running.
11. **Limits of cropping, exporting pictures and document properties.** Cropping does not delete content: the cropped-away parts remain in the file (use Redact to remove them for real). Each page has one crop box. "Cut the white margins" works by drawing the page at low resolution, so near-white content (very faint watermarks, for example), where all three colour components are above 244, is treated as blank. When exporting pictures, large pages are written in strips. In very rare cases, fewer than 0.2% of the pixels at the edges of text differ from drawing the whole page at once; the difference cannot be seen by eye. JPEG uses a fixed coding table, so the output is slightly larger than it would be with per-picture optimisation. Extracting pictures does not handle small pictures written directly into the page content (inline images). In the reader, exporting or extracting pictures replaces a file of the same name without asking; the command line needs `--force`. The font list in Document properties is read from the font resources of the pages and forms, and it is limited to 300 fonts and 40 milliseconds. For very large or badly formed files the list is incomplete, and the window says so. Changing properties does not change the Created and Modified times. Control characters such as line breaks in text the file itself contains (the creator, producer, title and so on) are shown as spaces in the window. The creator and producer show at most the first 200 characters. In the window, a line is at most 60 characters wide, and longer text is cut with "……". If the window is too tall for the screen, it is shrunk to the work area, and the OK and Cancel buttons always stay at the bottom of the window.
12. **Limits of the first stage of the interface redesign.** When the command bar is too narrow for the search box (a window under about 700 pixels wide), the search box disappears. Ctrl+F still works, and the find box floats over the page. The minimum window size is 640×420 (96 dpi). Tool icons use Windows' icon font (Segoe Fluent Icons; Segoe MDL2 Assets on Windows 10). There is no menu item for the accent colour setting `accent=system`. It has not been checked item by item on real Windows 10, with the high-contrast theme, or at every display scale other than 100% (see decisions.md). (Screen readers: while one is running, the window, menus and dialogs use the system's own parts, as described under "Screen readers" above. They have not been listened to item by item with real Narrator, NVDA or JAWS; the structure was only checked in a real window, see decisions.md.)
13. **Limits of the second stage of the interface redesign.** Windows' own dialogs (Open, Save, Print, choose a folder, and the message boxes that ask "Save changes?" and so on) still use the Windows look, so they stay light even in dark mode. In a dialog where you choose one of several items, the list opens as a menu drawn by the reader. Its width follows its text, so it may not match the width of the dialog. In a list box, ↑ and ↓ change the item, and F4 or Space opens it, but unlike Windows lists you cannot type to search while it is open. The pale background of the row under the mouse in the bookmark tree comes from Windows telling the reader about it, and it has not been checked on every version of Windows. When the sidebar slides out, the pages have already been laid out at the new width, so the sliding card lies over the page. When the window is maximised with the taskbar set to auto-hide, the window leaves 1 pixel on that edge so the taskbar can still pop up. This has only been checked with unit tests, because the taskbar settings could not be changed on this machine; it has not really been tried. The 轻 logo at 16, 24 and 32 pixels was drawn by hand. Other sizes (including the 30-pixel logo in the title bar at 125% scaling) are drawn as vectors and are not as crisp.

## 6. Licence

qingpdf is released under the MIT or Apache-2.0 licence, at your option (see LICENSE-MIT and LICENSE-APACHE). Third-party software and data, with their licences, are listed in THIRD-PARTY-NOTICES.md.
