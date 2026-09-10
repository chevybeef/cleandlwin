//! Opt-in tests use disposable files and a temporary native window.
use super::*;
use std::time::{Duration, Instant};

unsafe fn pump_until(mut ready: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ready() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "UI operation timed out"
        );
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "Opens a temporary native window; run explicitly on a Windows desktop"]
fn native_window_preview_settings_and_dpi() {
    let _com = platform::Com::init().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("Downloads");
    let destination = temp.path().join("Sorted");
    std::fs::create_dir(&source).unwrap();
    for name in [
        "invoice-2026.pdf",
        "Screenshot-holiday.png",
        "Résumé.txt",
        "archive.zip",
        "old.tmp",
        "active.part",
    ] {
        std::fs::write(source.join(name), b"sample").unwrap();
    }
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
        let class = wide("CleanDLWin.Smoke");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
            ..std::mem::zeroed()
        };
        assert_ne!(RegisterClassW(&wc), 0);
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class.as_ptr(),
            wide("CleanDL — Demo preview").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            60,
            60,
            1120,
            700,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        assert!(!hwnd.is_null());
        let prefs = Preferences {
            source: source.display().to_string(),
            destination: destination.display().to_string(),
            server_folders: false,
            ..Preferences::default()
        };
        let state = Box::new(RefCell::new(
            App::new(hwnd, prefs, temp.path().join("settings")).unwrap(),
        ));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const RefCell<App> as isize);
        {
            let app = state.borrow();
            app.layout();
            app.update_commands();
            assert_eq!(SendMessageW(app.toolbar, TB_BUTTONCOUNT, 0, 0), 4);
            assert!(!GetMenu(hwnd).is_null());
        }
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        UpdateWindow(hwnd);
        SetTimer(hwnd, 1, 25, None);
        // Switching to server defaults must not display or overwrite saved
        // local overrides. With no cache yet, the fields explain Preview.
        let defaults = state.borrow().defaults;
        set_check(defaults, true);
        SendMessageW(hwnd, WM_COMMAND, SERVER_FOLDERS as usize, 0);
        assert!(text(state.borrow().destination).starts_with("From server"));
        assert_eq!(
            state.borrow().prefs.destination,
            destination.display().to_string()
        );
        set_check(defaults, false);
        SendMessageW(hwnd, WM_COMMAND, SERVER_FOLDERS as usize, 0);
        assert_eq!(
            text(state.borrow().destination),
            destination.display().to_string()
        );
        SendMessageW(hwnd, WM_COMMAND, DEMO as usize, 0);
        pump_until(|| state.borrow().worker.is_none());
        {
            let app = state.borrow();
            assert_eq!(SendMessageW(app.list, LVM_GETITEMCOUNT, 0, 0), 6);
            assert_eq!(app.plan.as_ref().unwrap().counts(), (4, 1));
            assert_ne!(
                GetMenuState(GetMenu(hwnd), APPLY as u32, MF_BYCOMMAND) & MF_GRAYED,
                0
            );
            assert_eq!(std::fs::read_dir(&source).unwrap().count(), 6);
            assert!(!destination.exists());
            capture(hwnd, "target/ui-smoke.bmp");
        }
        let settings = thread::spawn(|| {
            let started = Instant::now();
            loop {
                let dialog = FindWindowW(wide("CleanDLWin.Settings").as_ptr(), null());
                if !dialog.is_null() && IsWindowVisible(dialog) != 0 {
                    assert!(!GetDlgItem(dialog, 301).is_null());
                    assert_ne!(
                        GetWindowLongPtrW(GetDlgItem(dialog, 302), GWL_STYLE)
                            & ES_PASSWORD as isize,
                        0
                    );
                    UpdateWindow(dialog);
                    capture(dialog, "target/settings-smoke.bmp");
                    PostMessageW(dialog, WM_COMMAND, IDCANCEL as usize, 0);
                    break;
                }
                assert!(started.elapsed() < Duration::from_secs(10));
                thread::sleep(Duration::from_millis(20));
            }
        });
        SendMessageW(hwnd, WM_COMMAND, SETTINGS as usize, 0);
        settings.join().unwrap();
        // Exercise the real Apply confirmation and worker, using only this
        // test's reviewed fixture. Recycling has its own opt-in shell test.
        {
            let mut app = state.borrow_mut();
            let plan = app.plan.as_mut().unwrap();
            plan.demo = false;
            plan.trial = false;
            for item in &mut plan.items {
                if item.action == Action::Recycle {
                    item.action = Action::Skip("Test leaves recycling to the shell test".into());
                }
            }
            app.update_commands();
            assert_eq!(
                GetMenuState(GetMenu(hwnd), APPLY as u32, MF_BYCOMMAND) & MF_GRAYED,
                0
            );
        }
        let confirmation = thread::spawn(|| {
            let started = Instant::now();
            loop {
                let dialog = FindWindowW(wide("#32770").as_ptr(), wide("CleanDL").as_ptr());
                let mut process = 0;
                if !dialog.is_null() {
                    GetWindowThreadProcessId(dialog, &mut process);
                }
                if process == std::process::id() {
                    PostMessageW(dialog, WM_COMMAND, IDYES as usize, 0);
                    break;
                }
                assert!(started.elapsed() < Duration::from_secs(10));
                thread::sleep(Duration::from_millis(20));
            }
        });
        SendMessageW(hwnd, WM_COMMAND, APPLY as usize, 0);
        confirmation.join().unwrap();
        pump_until(|| state.borrow().worker.is_none());
        assert!(destination.join("Archives/archive.zip").exists());
        assert_eq!(std::fs::read_dir(&source).unwrap().count(), 2);
        assert!(state.borrow().consumed);
        let rect = RECT {
            left: 50,
            top: 50,
            right: 1250,
            bottom: 850,
        };
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            144 | (144 << 16),
            &rect as *const _ as isize,
        );
        assert_eq!(state.borrow().dpi, 144);
        let source_edit = state.borrow().source;
        set_text(source_edit, &source.join("changed").display().to_string());
        assert!(state.borrow().plan.is_none());
        assert_ne!(
            GetMenuState(GetMenu(hwnd), APPLY as u32, MF_BYCOMMAND) & MF_GRAYED,
            0
        );
        KillTimer(hwnd, 1);
        DestroyWindow(hwnd);
        DeleteObject(state.borrow().font);
    }
}

unsafe fn capture(hwnd: HWND, path: &str) {
    unsafe extern "system" {
        fn PrintWindow(hwnd: HWND, dc: HDC, flags: u32) -> i32;
    }
    let mut rect: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut rect);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let dc = GetWindowDC(hwnd);
    let memory = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, width, height);
    let old = SelectObject(memory, bitmap);
    assert_ne!(PrintWindow(hwnd, memory, 2), 0);
    SelectObject(memory, old);
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    assert_ne!(
        GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr() as _,
            &mut info,
            DIB_RGB_COLORS
        ),
        0
    );
    let mut bmp = Vec::new();
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(54u32 + pixels.len() as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(std::slice::from_raw_parts(
        &info.bmiHeader as *const _ as *const u8,
        40,
    ));
    bmp.extend_from_slice(&pixels);
    std::fs::write(path, bmp).unwrap();
    DeleteObject(bitmap);
    DeleteDC(memory);
    ReleaseDC(hwnd, dc);
}
