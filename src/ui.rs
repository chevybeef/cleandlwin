//! UI handles and state are owned by the main thread. Workers send plain data.
#![allow(unsafe_op_in_unsafe_fn)]
use crate::{
    config::{self, Preferences},
    engine::{self, Action, Event, Plan, Summary},
    platform::{self, wide},
};
use anyhow::{Result, bail};
use std::{
    cell::RefCell,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

const PREVIEW: u16 = 101;
const APPLY: u16 = 102;
const CANCEL: u16 = 103;
const SETTINGS: u16 = 104;
const EXIT: u16 = 105;
const LOG: u16 = 106;
const RULES: u16 = 107;
const DEMO: u16 = 108;
const ABOUT: u16 = 109;
const SOURCE: u16 = 201;
const DESTINATION: u16 = 202;
const BROWSE_SOURCE: u16 = 203;
const BROWSE_DESTINATION: u16 = 204;
const SERVER_FOLDERS: u16 = 205;
const FORM_COMMAND: u32 = WM_APP + 20;

enum WorkerMessage {
    Event(Event),
    Preview(Result<Plan>),
    Applied(Result<Summary>),
    Connection(Result<String>),
}
struct App {
    hwnd: HWND,
    toolbar: HWND,
    list: HWND,
    status: HWND,
    progress: HWND,
    source_label: HWND,
    destination_label: HWND,
    source: HWND,
    destination: HWND,
    browse_source: HWND,
    browse_destination: HWND,
    defaults: HWND,
    font: HFONT,
    dpi: u32,
    prefs: Preferences,
    dir: PathBuf,
    plan: Option<Plan>,
    consumed: bool,
    worker: Option<Receiver<WorkerMessage>>,
    cancel: Arc<AtomicBool>,
    closing: bool,
}
fn low(value: usize) -> u16 {
    (value & 0xffff) as u16
}
fn high(value: usize) -> u16 {
    ((value >> 16) & 0xffff) as u16
}
pub fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("CleanDL").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
unsafe fn message(owner: HWND, text: &str, flags: u32) -> i32 {
    MessageBoxW(owner, wide(text).as_ptr(), wide("CleanDL").as_ptr(), flags)
}
unsafe fn text(hwnd: HWND) -> String {
    let mut buffer = vec![0u16; GetWindowTextLengthW(hwnd) as usize + 1];
    let len = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
    String::from_utf16_lossy(&buffer[..len as usize])
}
unsafe fn set_text(hwnd: HWND, value: &str) {
    SetWindowTextW(hwnd, wide(value).as_ptr());
}
unsafe fn check(hwnd: HWND) -> bool {
    SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize
}
unsafe fn set_check(hwnd: HWND, checked: bool) {
    SendMessageW(
        hwnd,
        BM_SETCHECK,
        if checked { BST_CHECKED } else { BST_UNCHECKED } as usize,
        0,
    );
}
unsafe fn child(parent: HWND, class: &str, label: &str, style: u32, id: u16, font: HFONT) -> HWND {
    let hwnd = CreateWindowExW(
        if class == "EDIT" { WS_EX_CLIENTEDGE } else { 0 },
        wide(class).as_ptr(),
        wide(label).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        0,
        0,
        0,
        0,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(null()),
        null(),
    );
    SendMessageW(hwnd, WM_SETFONT, font as usize, 1);
    hwnd
}
unsafe fn font(dpi: u32) -> HFONT {
    CreateFontW(
        -((9 * dpi / 72) as i32),
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        0,
        0,
        CLEARTYPE_QUALITY as u32,
        0,
        wide("Segoe UI").as_ptr(),
    )
}
unsafe fn append(menu: HMENU, id: u16, label: &str) {
    AppendMenuW(menu, MF_STRING, id as usize, wide(label).as_ptr());
}
unsafe fn menus(hwnd: HWND) {
    let bar = CreateMenu();
    let file = CreatePopupMenu();
    let tools = CreatePopupMenu();
    let help = CreatePopupMenu();
    append(file, PREVIEW, "&Preview\tF5");
    append(file, APPLY, "&Apply…\tCtrl+Enter");
    append(file, CANCEL, "&Cancel\tEsc");
    AppendMenuW(file, MF_SEPARATOR, 0, null());
    append(file, EXIT, "E&xit");
    append(tools, SETTINGS, "&Settings…\tCtrl+,");
    append(tools, LOG, "Open &Log");
    append(tools, RULES, "Manage &Rules…");
    append(help, DEMO, "&Demo preview");
    append(help, ABOUT, "&About CleanDL");
    AppendMenuW(bar, MF_POPUP, file as usize, wide("&File").as_ptr());
    AppendMenuW(bar, MF_POPUP, tools as usize, wide("&Tools").as_ptr());
    AppendMenuW(bar, MF_POPUP, help as usize, wide("&Help").as_ptr());
    SetMenu(hwnd, bar);
}
pub fn run() -> Result<()> {
    let _com = platform::Com::init()?;
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES
                | ICC_LISTVIEW_CLASSES
                | ICC_PROGRESS_CLASS
                | ICC_STANDARD_CLASSES,
        });
        let instance = GetModuleHandleW(null());
        let class = wide("CleanDLWin.Main");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
            hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
            ..std::mem::zeroed()
        };
        if RegisterClassW(&wc) == 0 {
            bail!("Could not register window class");
        }
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class.as_ptr(),
            wide("CleanDL — Downloads organiser").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1120,
            700,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            bail!("Could not create main window");
        }
        let dir = config::data_dir()?;
        let first_run = !dir.join("settings.json").exists();
        let prefs = match config::load_preferences(&dir) {
            Ok(prefs) => prefs,
            Err(e) => {
                message(
                    hwnd,
                    &format!(
                        "Could not load settings: {e:#}\nPlease re-enter your connection settings."
                    ),
                    MB_OK | MB_ICONWARNING,
                );
                Preferences::default()
            }
        };
        let state = Box::new(RefCell::new(App::new(hwnd, prefs, dir)?));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const RefCell<App> as isize);
        {
            let app = state.borrow();
            app.layout();
            app.update_commands();
        }
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetTimer(hwnd, 1, 50, None);
        if first_run {
            let mut app = state.borrow_mut();
            match config::cli_import() {
                Ok(Some(imported))
                    if message(
                        hwnd,
                        "Import the server URL and API key from your existing CleanDL CLI installation?\n\nThe CLI’s files will remain unchanged.",
                        MB_YESNO | MB_ICONQUESTION,
                    ) == IDYES =>
                {
                    app.prefs = imported;
                }
                Err(e) => {
                    message(
                        hwnd,
                        &format!("Could not read CLI settings: {e:#}"),
                        MB_OK | MB_ICONWARNING,
                    );
                }
                _ => {}
            }
            app.settings();
        }
        let accelerators = [
            ACCEL {
                fVirt: FVIRTKEY,
                key: VK_F5,
                cmd: PREVIEW,
            },
            ACCEL {
                fVirt: FVIRTKEY | FCONTROL,
                key: VK_RETURN,
                cmd: APPLY,
            },
            ACCEL {
                fVirt: FVIRTKEY,
                key: VK_ESCAPE,
                cmd: CANCEL,
            },
            ACCEL {
                fVirt: FVIRTKEY | FCONTROL,
                key: VK_OEM_COMMA,
                cmd: SETTINGS,
            },
        ];
        let accel = CreateAcceleratorTableW(accelerators.as_ptr(), accelerators.len() as i32);
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let result = GetMessageW(&mut msg, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            if TranslateAcceleratorW(hwnd, accel, &msg) == 0 && IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        DestroyAcceleratorTable(accel);
        DeleteObject(state.borrow().font);
        Ok(())
    }
}
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // Native controls can synchronously notify during an update. try_borrow_mut
    // ignores those nested notifications instead of aliasing mutable state.
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
    if !pointer.is_null()
        && let Ok(mut app) = (*pointer).try_borrow_mut()
    {
        match msg {
            WM_SIZE => {
                app.layout();
                return 0;
            }
            WM_DPICHANGED => {
                app.dpi = high(w) as u32;
                let old = app.font;
                app.font = font(app.dpi);
                for h in app.controls() {
                    SendMessageW(h, WM_SETFONT, app.font as usize, 1);
                }
                DeleteObject(old);
                let rect = &*(l as *const RECT);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                app.layout();
                return 0;
            }
            WM_GETMINMAXINFO => {
                let info = &mut *(l as *mut MINMAXINFO);
                info.ptMinTrackSize.x = app.scale(760);
                info.ptMinTrackSize.y = app.scale(420);
                return 0;
            }
            WM_COMMAND => {
                app.command(low(w), high(w));
                return 0;
            }
            WM_TIMER => {
                app.poll();
                return 0;
            }
            WM_CLOSE => {
                if app.worker.is_some() {
                    app.closing = true;
                    app.cancel.store(true, Ordering::Relaxed);
                    app.status_text("Cancelling before closing…");
                } else {
                    DestroyWindow(hwnd);
                }
                return 0;
            }
            _ => {}
        }
    }
    if msg == WM_DESTROY {
        PostQuitMessage(0);
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}
impl App {
    unsafe fn new(hwnd: HWND, prefs: Preferences, dir: PathBuf) -> Result<Self> {
        let dpi = GetDpiForWindow(hwnd);
        let font = font(dpi);
        menus(hwnd);
        let toolbar = child(
            hwnd,
            "ToolbarWindow32",
            "",
            TBSTYLE_FLAT | TBSTYLE_TOOLTIPS | CCS_TOP as u32,
            0,
            font,
        );
        SendMessageW(
            toolbar,
            TB_BUTTONSTRUCTSIZE,
            std::mem::size_of::<TBBUTTON>(),
            0,
        );
        let bitmap = TBADDBITMAP {
            hInst: -1isize as HINSTANCE,
            nID: IDB_STD_SMALL_COLOR as usize,
        };
        SendMessageW(toolbar, TB_ADDBITMAP, 0, &bitmap as *const _ as isize);
        let labels = [
            wide("Preview"),
            wide("Apply"),
            wide("Cancel"),
            wide("Settings"),
        ];
        let buttons: Vec<TBBUTTON> = [
            (PREVIEW, STD_FIND),
            (APPLY, STD_FILEOPEN),
            (CANCEL, STD_DELETE),
            (SETTINGS, STD_PROPERTIES),
        ]
        .iter()
        .zip(labels.iter())
        .map(|((id, image), label)| TBBUTTON {
            iBitmap: *image as i32,
            idCommand: *id as i32,
            fsState: TBSTATE_ENABLED as u8,
            fsStyle: (BTNS_BUTTON | BTNS_AUTOSIZE | BTNS_SHOWTEXT) as u8,
            iString: label.as_ptr() as isize,
            ..std::mem::zeroed()
        })
        .collect();
        SendMessageW(
            toolbar,
            TB_ADDBUTTONSW,
            buttons.len(),
            buttons.as_ptr() as isize,
        );
        SendMessageW(toolbar, TB_AUTOSIZE, 0, 0);
        let source_label = child(hwnd, "STATIC", "Source folder:", 0, 0, font);
        let destination_label = child(hwnd, "STATIC", "Destination base:", 0, 0, font);
        let (source_text, destination_text) = config::folder_display(&prefs, &dir);
        let source = child(
            hwnd,
            "EDIT",
            &source_text,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            SOURCE,
            font,
        );
        let destination = child(
            hwnd,
            "EDIT",
            &destination_text,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            DESTINATION,
            font,
        );
        let browse_source = child(hwnd, "BUTTON", "Browse…", WS_TABSTOP, BROWSE_SOURCE, font);
        let browse_destination = child(
            hwnd,
            "BUTTON",
            "Browse…",
            WS_TABSTOP,
            BROWSE_DESTINATION,
            font,
        );
        let defaults = child(
            hwnd,
            "BUTTON",
            "Use &server folder defaults",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            SERVER_FOLDERS,
            font,
        );
        set_check(defaults, prefs.server_folders);
        let list = child(
            hwnd,
            "SysListView32",
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SHOWSELALWAYS,
            0,
            font,
        );
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_GRIDLINES | LVS_EX_LABELTIP)
                as isize,
        );
        for (index, (label, width)) in [
            ("File", 190),
            ("Size", 85),
            ("Action", 75),
            ("Destination", 330),
            ("Status", 280),
        ]
        .iter()
        .enumerate()
        {
            let mut name = wide(label);
            let column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
                cx: width * dpi as i32 / 96,
                pszText: name.as_mut_ptr(),
                iSubItem: index as i32,
                ..std::mem::zeroed()
            };
            SendMessageW(list, LVM_INSERTCOLUMNW, index, &column as *const _ as isize);
        }
        let status = child(
            hwnd,
            "msctls_statusbar32",
            "Ready — Preview to review files, or Help > Demo to try offline.",
            SBARS_SIZEGRIP,
            0,
            font,
        );
        let progress = child(status, "msctls_progress32", "", PBS_SMOOTH, 0, font);
        SendMessageW(progress, PBM_SETRANGE32, 0, 1000);
        if [toolbar, list, status, progress, source, destination]
            .iter()
            .any(|h| h.is_null())
        {
            bail!("Could not create native controls");
        }
        Ok(Self {
            hwnd,
            toolbar,
            list,
            status,
            progress,
            source_label,
            destination_label,
            source,
            destination,
            browse_source,
            browse_destination,
            defaults,
            font,
            dpi,
            prefs,
            dir,
            plan: None,
            consumed: false,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            closing: false,
        })
    }
    fn scale(&self, value: i32) -> i32 {
        value * self.dpi as i32 / 96
    }
    fn controls(&self) -> Vec<HWND> {
        vec![
            self.toolbar,
            self.list,
            self.status,
            self.source_label,
            self.destination_label,
            self.source,
            self.destination,
            self.browse_source,
            self.browse_destination,
            self.defaults,
        ]
    }
    unsafe fn layout(&self) {
        let mut rect: RECT = std::mem::zeroed();
        GetClientRect(self.hwnd, &mut rect);
        let width = rect.right;
        let height = rect.bottom;
        SendMessageW(self.toolbar, TB_AUTOSIZE, 0, 0);
        SendMessageW(self.status, WM_SIZE, 0, 0);
        let mut bar: RECT = std::mem::zeroed();
        GetWindowRect(self.toolbar, &mut bar);
        let top = bar.bottom - bar.top + self.scale(10);
        let mut status: RECT = std::mem::zeroed();
        GetWindowRect(self.status, &mut status);
        let bottom = status.bottom - status.top;
        for (row, label, edit, browse) in [
            (0, self.source_label, self.source, self.browse_source),
            (
                1,
                self.destination_label,
                self.destination,
                self.browse_destination,
            ),
        ] {
            let y = top + self.scale(row * 32);
            MoveWindow(
                label,
                self.scale(12),
                y + self.scale(4),
                self.scale(115),
                self.scale(22),
                1,
            );
            MoveWindow(
                edit,
                self.scale(132),
                y,
                (width - self.scale(234)).max(10),
                self.scale(25),
                1,
            );
            MoveWindow(
                browse,
                width - self.scale(94),
                y,
                self.scale(82),
                self.scale(25),
                1,
            );
        }
        MoveWindow(
            self.defaults,
            self.scale(132),
            top + self.scale(64),
            self.scale(280),
            self.scale(24),
            1,
        );
        let list_top = top + self.scale(98);
        MoveWindow(
            self.list,
            self.scale(12),
            list_top,
            (width - self.scale(24)).max(10),
            (height - list_top - bottom - self.scale(10)).max(10),
            1,
        );
        let parts = [(width - self.scale(230)).max(100), -1];
        SendMessageW(self.status, SB_SETPARTS, 2, parts.as_ptr() as isize);
        let mut part: RECT = std::mem::zeroed();
        SendMessageW(self.status, SB_GETRECT, 1, &mut part as *mut _ as isize);
        MoveWindow(
            self.progress,
            part.left + 4,
            part.top + 3,
            (part.right - part.left - self.scale(22)).max(10),
            (part.bottom - part.top - 6).max(8),
            1,
        );
    }
    unsafe fn status_text(&self, value: &str) {
        SendMessageW(self.status, SB_SETTEXTW, 0, wide(value).as_ptr() as isize);
    }
    unsafe fn update_commands(&self) {
        let busy = self.worker.is_some();
        let apply = !busy
            && !self.consumed
            && self.plan.as_ref().is_some_and(|p| {
                !p.demo && !p.trial && p.items.iter().any(|i| i.action.actionable())
            });
        for (id, enabled) in [
            (PREVIEW, !busy),
            (APPLY, apply),
            (CANCEL, busy),
            (SETTINGS, !busy),
            (DEMO, !busy),
        ] {
            EnableMenuItem(
                GetMenu(self.hwnd),
                id as u32,
                MF_BYCOMMAND | if enabled { MF_ENABLED } else { MF_GRAYED },
            );
            SendMessageW(self.toolbar, TB_ENABLEBUTTON, id as usize, enabled as isize);
        }
        for h in [
            self.source,
            self.destination,
            self.browse_source,
            self.browse_destination,
        ] {
            EnableWindow(h, (!busy && !self.prefs.server_folders) as i32);
        }
        EnableWindow(self.defaults, (!busy) as i32);
        DrawMenuBar(self.hwnd);
    }
    unsafe fn invalidate(&mut self) {
        self.plan = None;
        self.consumed = false;
        SendMessageW(self.list, LVM_DELETEALLITEMS, 0, 0);
        self.status_text("Settings changed — preview again.");
        self.update_commands();
    }
    unsafe fn read_folders(&mut self) {
        self.prefs.server_folders = check(self.defaults);
        if !self.prefs.server_folders {
            self.prefs.source = text(self.source);
            self.prefs.destination = text(self.destination);
        }
    }
    unsafe fn show_folders(&self) {
        let (source, destination) = config::folder_display(&self.prefs, &self.dir);
        set_text(self.source, &source);
        set_text(self.destination, &destination);
    }
    unsafe fn command(&mut self, id: u16, notification: u16) {
        if [SOURCE, DESTINATION].contains(&id)
            && notification == EN_CHANGE as u16
            && self.worker.is_none()
        {
            self.read_folders();
            self.invalidate();
            return;
        }
        match id {
            PREVIEW | DEMO if self.worker.is_none() => self.start_preview(id == DEMO),
            APPLY if self.worker.is_none() => self.start_apply(),
            CANCEL => {
                self.cancel.store(true, Ordering::Relaxed);
                self.status_text("Cancelling…");
            }
            SETTINGS if self.worker.is_none() => self.settings(),
            SERVER_FOLDERS if self.worker.is_none() => {
                self.prefs.server_folders = check(self.defaults);
                self.show_folders();
                self.invalidate();
            }
            BROWSE_SOURCE | BROWSE_DESTINATION if self.worker.is_none() => {
                match platform::choose_folder(self.hwnd as isize) {
                    Ok(Some(folder)) => {
                        set_text(
                            if id == BROWSE_SOURCE {
                                self.source
                            } else {
                                self.destination
                            },
                            &folder,
                        );
                        self.read_folders();
                        self.invalidate();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        message(
                            self.hwnd,
                            &format!("Cannot browse folders: {e:#}"),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
            }
            LOG => {
                config::log(&self.dir, "Log opened");
                if let Err(e) = platform::open(
                    self.hwnd as isize,
                    &self.dir.join("cleandlwin.log").display().to_string(),
                ) {
                    message(self.hwnd, &e.to_string(), MB_OK | MB_ICONERROR);
                }
            }
            RULES => {
                if let Err(e) =
                    platform::open(self.hwnd as isize, &config::website(&self.prefs.url))
                {
                    message(self.hwnd, &e.to_string(), MB_OK | MB_ICONERROR);
                }
            }
            ABOUT => {
                message(
                    self.hwnd,
                    "CleanDL for Windows 0.1.0\n\nNative Windows downloads organiser.\nSorting engine adapted from cleandl-rs.\nMIT © 2026 CleanDL contributors.\n\nPreview first, then apply reviewed moves and recycling.",
                    MB_OK | MB_ICONINFORMATION,
                );
            }
            EXIT => {
                PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
            }
            _ => {}
        }
    }
    unsafe fn settings(&mut self) {
        match settings_dialog(self.hwnd, self.font, &self.prefs) {
            Ok(Some(prefs)) => match config::save_preferences(&self.dir, &prefs) {
                Ok(()) => {
                    self.prefs = prefs;
                    self.show_folders();
                    set_check(self.defaults, self.prefs.server_folders);
                    self.invalidate();
                }
                Err(e) => {
                    message(
                        self.hwnd,
                        &format!("Could not save settings: {e:#}"),
                        MB_OK | MB_ICONERROR,
                    );
                }
            },
            Ok(None) => {}
            Err(e) => {
                message(
                    self.hwnd,
                    &format!("Could not open Settings: {e:#}"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }
    unsafe fn begin(&mut self, receiver: Receiver<WorkerMessage>) {
        self.worker = Some(receiver);
        self.cancel.store(false, Ordering::Relaxed);
        self.update_commands();
        SetWindowLongPtrW(
            self.progress,
            GWL_STYLE,
            GetWindowLongPtrW(self.progress, GWL_STYLE) | PBS_MARQUEE as isize,
        );
        SendMessageW(self.progress, PBM_SETMARQUEE, 1, 30);
    }
    unsafe fn start_preview(&mut self, demo: bool) {
        self.read_folders();
        if let Err(e) = config::save_preferences(&self.dir, &self.prefs) {
            // This happens before the worker starts, so it must be logged here.
            let error = format!("Could not save preferences: {e:#}");
            config::log(&self.dir, &error);
            message(self.hwnd, &error, MB_OK | MB_ICONERROR);
            return;
        }
        self.invalidate();
        let (sender, receiver) = mpsc::channel();
        self.begin(receiver);
        self.status_text("Preparing preview…");
        let prefs = self.prefs.clone();
        let dir = self.dir.clone();
        let cancel = self.cancel.clone();
        thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                config::prepare(&prefs, &dir, demo, &cancel, &mut |event| {
                    let _ = sender.send(WorkerMessage::Event(event));
                })
            })
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Preview worker stopped unexpectedly")));
            let _ = sender.send(WorkerMessage::Preview(result));
        });
    }
    unsafe fn start_apply(&mut self) {
        let Some(plan) = self
            .plan
            .as_ref()
            .filter(|p| !p.trial && !p.demo && !self.consumed)
        else {
            return;
        };
        let (moves, recycle) = plan.counts();
        if moves + recycle == 0 {
            return;
        }
        let cached = if plan.cached {
            "\n\nThe server was unavailable. This run uses saved settings. Continue with this cached configuration?"
        } else {
            ""
        };
        if message(
            self.hwnd,
            &format!(
                "Apply this preview?\n\nMove {moves} file(s).\nSend {recycle} file(s) to the Recycle Bin.\n\nChanged files and occupied destinations will not be processed.{cached}"
            ),
            MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
        ) != IDYES
        {
            return;
        }
        let plan = plan.clone();
        self.consumed = true;
        let cancel = self.cancel.clone();
        let dir = self.dir.clone();
        let debug = self.prefs.debug;
        let (sender, receiver) = mpsc::channel();
        self.begin(receiver);
        self.status_text("Applying reviewed actions…");
        thread::spawn(move || {
            let mut last_progress = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine::apply(&plan, &cancel, &mut |event| {
                if let Event::Item { index, status } = &event { config::log(&dir, &format!("{}: {status}", plan.items[*index].source.display())); }
                if let Event::Progress { bytes, size, .. } = &event { if *bytes != 0 && bytes != size && last_progress.elapsed().as_millis() < 50 { return; } last_progress = Instant::now(); }
                if debug && let Event::Phase(phase) = &event { config::log(&dir, phase); }
                let _ = sender.send(WorkerMessage::Event(event));
            }))).unwrap_or_else(|_| Err(anyhow::anyhow!("Apply worker stopped unexpectedly; inspect the log and destinations before retrying")));
            let _ = sender.send(WorkerMessage::Applied(result));
        });
    }
    unsafe fn cell(&self, row: usize, column: i32, value: &str) {
        let mut value = wide(value);
        let item = LVITEMW {
            iSubItem: column,
            pszText: value.as_mut_ptr(),
            ..std::mem::zeroed()
        };
        SendMessageW(self.list, LVM_SETITEMTEXTW, row, &item as *const _ as isize);
    }
    unsafe fn populate(&self, plan: &Plan) {
        SendMessageW(self.list, WM_SETREDRAW, 0, 0);
        SendMessageW(self.list, LVM_DELETEALLITEMS, 0, 0);
        for (index, item) in plan.items.iter().enumerate() {
            let mut name = wide(item.source.file_name().unwrap_or_default());
            let row = LVITEMW {
                mask: LVIF_TEXT,
                iItem: index as i32,
                pszText: name.as_mut_ptr(),
                ..std::mem::zeroed()
            };
            SendMessageW(self.list, LVM_INSERTITEMW, 0, &row as *const _ as isize);
            self.cell(index, 1, &format_size(item.size));
            self.cell(index, 2, item.action.label());
            self.cell(
                index,
                3,
                &match &item.action {
                    Action::Move(path) => path.display().to_string(),
                    Action::Recycle => "Recycle Bin".into(),
                    Action::Skip(_) => String::new(),
                },
            );
            self.cell(index, 4, &item.status);
        }
        SendMessageW(self.list, WM_SETREDRAW, 1, 0);
        InvalidateRect(self.list, null(), 1);
    }
    unsafe fn determinate(&self) {
        SendMessageW(self.progress, PBM_SETMARQUEE, 0, 0);
        SetWindowLongPtrW(
            self.progress,
            GWL_STYLE,
            GetWindowLongPtrW(self.progress, GWL_STYLE) & !(PBS_MARQUEE as isize),
        );
    }
    unsafe fn poll(&mut self) {
        let mut messages = Vec::new();
        let mut disconnected = false;
        if let Some(worker) = &self.worker {
            for _ in 0..256 {
                match worker.try_recv() {
                    Ok(m) => messages.push(m),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for msg in messages {
            match msg {
                WorkerMessage::Event(Event::Phase(phase)) => {
                    self.status_text(&phase);
                    if self.prefs.debug {
                        config::log(&self.dir, &phase);
                    }
                }
                WorkerMessage::Event(Event::Progress {
                    index,
                    total,
                    bytes,
                    size,
                    name,
                }) => {
                    self.determinate();
                    let position = if size == 0 {
                        1000
                    } else {
                        ((bytes as f64 / size as f64) * 1000.0).min(1000.0) as usize
                    };
                    SendMessageW(self.progress, PBM_SETPOS, position, 0);
                    self.status_text(&format!(
                        "File {} of {total} — {} / {} — {name}",
                        index + 1,
                        format_size(bytes),
                        format_size(size)
                    ));
                }
                WorkerMessage::Event(Event::Item { index, status }) => {
                    if let Some(plan) = &mut self.plan
                        && let Some(item) = plan.items.get_mut(index)
                    {
                        item.status.clone_from(&status);
                    }
                    self.cell(index, 4, &status);
                }
                WorkerMessage::Preview(result) => {
                    self.finish();
                    match result {
                        Ok(plan) => {
                            self.populate(&plan);
                            if self.prefs.server_folders {
                                set_text(self.source, &plan.source.display().to_string());
                                set_text(self.destination, &plan.destination.display().to_string());
                            }
                            let (moves, recycle) = plan.counts();
                            let mode = if plan.demo {
                                "Demo · trial only"
                            } else if plan.trial {
                                "Trial only"
                            } else if plan.cached {
                                "Cached settings"
                            } else {
                                "Connected"
                            };
                            self.status_text(&format!(
                                "{mode} — {moves} moves, {recycle} to recycle, {} skipped",
                                plan.items.len() - moves - recycle
                            ));
                            config::log(
                                &self.dir,
                                &format!("Preview: {mode}; {moves} moves, {recycle} recycle"),
                            );
                            for warning in &plan.warnings {
                                config::log(&self.dir, warning);
                            }
                            config::log_preview_items(&self.dir, &plan, self.prefs.debug);
                            if !plan.warnings.is_empty() && !self.closing {
                                message(
                                    self.hwnd,
                                    &plan.warnings.join("\n\n"),
                                    MB_OK | MB_ICONWARNING,
                                );
                            }
                            self.plan = Some(plan);
                            self.update_commands();
                        }
                        Err(error) => self.report_error(error),
                    }
                }
                WorkerMessage::Applied(result) => {
                    self.finish();
                    match result {
                        Ok(summary) => {
                            let text = format!(
                                "{} — {} completed, {} skipped, {} failed. Preview again for another run.",
                                if summary.cancelled {
                                    "Cancelled"
                                } else {
                                    "Finished"
                                },
                                summary.completed,
                                summary.skipped,
                                summary.failed
                            );
                            self.status_text(&text);
                            config::log(&self.dir, &text);
                        }
                        Err(error) => self.report_error(error),
                    }
                }
                WorkerMessage::Connection(_) => {}
            }
        }
        if disconnected && self.worker.is_some() {
            self.finish();
            self.report_error(anyhow::anyhow!("Worker stopped without a result"));
        }
        if self.closing && self.worker.is_none() {
            DestroyWindow(self.hwnd);
        }
    }
    unsafe fn finish(&mut self) {
        self.worker = None;
        self.determinate();
        SendMessageW(self.progress, PBM_SETPOS, 0, 0);
        self.update_commands();
    }
    unsafe fn report_error(&self, error: anyhow::Error) {
        let value = format!("{error:#}");
        config::log(&self.dir, &value);
        self.status_text(&value);
        if !self.closing && !self.cancel.load(Ordering::Relaxed) {
            message(self.hwnd, &value, MB_OK | MB_ICONERROR);
        }
    }
}
fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KiB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0))
    }
}

unsafe extern "system" fn form_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_COMMAND {
        PostMessageW(hwnd, FORM_COMMAND, w, l);
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}
unsafe fn settings_dialog(
    owner: HWND,
    font: HFONT,
    prefs: &Preferences,
) -> Result<Option<Preferences>> {
    let instance = GetModuleHandleW(null());
    let class = wide("CleanDLWin.Settings");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(form_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
        ..std::mem::zeroed()
    };
    RegisterClassW(&wc);
    let dpi = GetDpiForWindow(owner);
    let s = |n: i32| n * dpi as i32 / 96;
    let mut owner_rect: RECT = std::mem::zeroed();
    GetWindowRect(owner, &mut owner_rect);
    let hwnd = CreateWindowExW(
        WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT,
        class.as_ptr(),
        wide("CleanDL Settings").as_ptr(),
        WS_CAPTION | WS_SYSMENU | WS_POPUP,
        owner_rect.left + s(50),
        owner_rect.top + s(50),
        s(620),
        s(380),
        owner,
        null_mut(),
        instance,
        null(),
    );
    if hwnd.is_null() {
        bail!("Could not create settings window");
    }
    let add = |class: &str, title: &str, style: u32, id: u16, x: i32, y: i32, w: i32, h: i32| {
        let control = child(hwnd, class, title, style, id, font);
        MoveWindow(control, s(x), s(y), s(w), s(h), 1);
        control
    };
    add("STATIC", "Server API URL:", 0, 0, 16, 20, 125, 22);
    let url = add(
        "EDIT",
        &prefs.url,
        WS_TABSTOP | ES_AUTOHSCROLL as u32,
        301,
        148,
        16,
        438,
        26,
    );
    add("STATIC", "API key:", 0, 0, 16, 58, 125, 22);
    let key = add(
        "EDIT",
        &prefs.key,
        WS_TABSTOP | (ES_AUTOHSCROLL | ES_PASSWORD) as u32,
        302,
        148,
        54,
        438,
        26,
    );
    add(
        "BUTTON",
        "Get a connection key…",
        WS_TABSTOP,
        303,
        148,
        92,
        195,
        28,
    );
    let test = add(
        "BUTTON",
        "Test connection",
        WS_TABSTOP,
        304,
        358,
        92,
        150,
        28,
    );
    add("STATIC", "Apply mode:", 0, 0, 16, 141, 125, 22);
    let trial = add(
        "COMBOBOX",
        "",
        WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
        305,
        148,
        136,
        438,
        160,
    );
    for label in [
        "Follow server trial-run setting",
        "Trial only — disable Apply",
        "Allow Apply after confirmation",
    ] {
        SendMessageW(trial, CB_ADDSTRING, 0, wide(label).as_ptr() as isize);
    }
    SendMessageW(
        trial,
        CB_SETCURSEL,
        match prefs.trial {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        0,
    );
    let debug = add(
        "BUTTON",
        "Enable debug logging",
        WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        306,
        148,
        180,
        320,
        24,
    );
    set_check(debug, prefs.debug);
    add(
        "STATIC",
        "Folder overrides are on the main window. Rules are managed on your server.\nYour API key is encrypted for your Windows account.",
        0,
        0,
        16,
        216,
        570,
        44,
    );
    let status = add("STATIC", "", 0, 0, 16, 266, 570, 24);
    let save = add(
        "BUTTON",
        "Save",
        WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        IDOK as u16,
        386,
        300,
        95,
        28,
    );
    add(
        "BUTTON",
        "Cancel",
        WS_TABSTOP,
        IDCANCEL as u16,
        491,
        300,
        95,
        28,
    );
    EnableWindow(owner, 0);
    ShowWindow(hwnd, SW_SHOW);
    SetFocus(url);
    SetTimer(hwnd, 1, 50, None);
    let mut result = None;
    let mut receiver: Option<Receiver<WorkerMessage>> = None;
    let cancel = Arc::new(AtomicBool::new(false));
    let read = || -> Result<Preferences> {
        let mut p = prefs.clone();
        p.url = config::normalize_url(&text(url))?;
        p.key = text(key).trim().into();
        p.debug = check(debug);
        p.trial = match SendMessageW(trial, CB_GETCURSEL, 0, 0) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        Ok(p)
    };
    let mut msg: MSG = std::mem::zeroed();
    while IsWindow(hwnd) != 0 {
        let got = GetMessageW(&mut msg, null_mut(), 0, 0);
        if got <= 0 {
            if got == 0 {
                PostQuitMessage(msg.wParam as i32);
            }
            break;
        }
        if msg.hwnd == hwnd && msg.message == FORM_COMMAND {
            match low(msg.wParam) {
                id if id == IDOK as u16 && receiver.is_none() => match read() {
                    Ok(p) => {
                        result = Some(p);
                        DestroyWindow(hwnd);
                    }
                    Err(e) => {
                        message(hwnd, &e.to_string(), MB_OK | MB_ICONWARNING);
                    }
                },
                id if id == IDCANCEL as u16 => {
                    DestroyWindow(hwnd);
                }
                303 => match config::normalize_url(&text(url))
                    .and_then(|url| platform::open(hwnd as isize, &config::login(&url)))
                {
                    Ok(()) => {}
                    Err(e) => {
                        message(hwnd, &e.to_string(), MB_OK | MB_ICONERROR);
                    }
                },
                304 if receiver.is_none() => match read() {
                    Ok(p) => {
                        let (sender, rx) = mpsc::channel();
                        receiver = Some(rx);
                        let cancel = cancel.clone();
                        set_text(status, "Connecting…");
                        EnableWindow(test, 0);
                        EnableWindow(save, 0);
                        thread::spawn(move || {
                            let result = config::test_connection(&p, &cancel);
                            let _ = sender.send(WorkerMessage::Connection(result));
                        });
                    }
                    Err(e) => {
                        message(hwnd, &e.to_string(), MB_OK | MB_ICONWARNING);
                    }
                },
                _ => {}
            }
        } else if IsDialogMessageW(hwnd, &msg) == 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(rx) = &receiver
            && let Ok(WorkerMessage::Connection(value)) = rx.try_recv()
        {
            set_text(status, &value.unwrap_or_else(|e| e.to_string()));
            receiver = None;
            EnableWindow(test, 1);
            EnableWindow(save, 1);
        }
    }
    cancel.store(true, Ordering::Relaxed);
    EnableWindow(owner, 1);
    SetForegroundWindow(owner);
    Ok(result)
}
