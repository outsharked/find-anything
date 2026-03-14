//! Win32 borderless popup window that lists the most recently indexed files.
//!
//! The popup appears above the taskbar at the bottom-right of the work area
//! when the user left-clicks the tray icon.  It dismisses automatically when
//! it loses activation (user clicks elsewhere) or when the user presses Escape.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;

use anyhow::Result;
use find_common::api::RecentFile;

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::CreateFontW;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
    IsWindowVisible, MoveWindow, RegisterClassExW, RegisterWindowMessageA, SendMessageW,
    SetForegroundWindow, SetWindowPos, ShowWindow, SystemParametersInfoW, WNDCLASSEXW,
    CS_DROPSHADOW, CS_HREDRAW, CS_VREDRAW, MSGFLT_ALLOW, SW_HIDE, SW_SHOW, SPI_GETWORKAREA,
    SWP_NOZORDER, WM_ACTIVATE, WM_KEYDOWN, WM_SIZE, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WS_VSCROLL,
};

const WM_COMMAND: u32 = 0x0111;
/// STATIC control styles.
const SS_CENTER: u32 = 0x0001;
const SS_NOPREFIX: u32 = 0x0080;

// LBS_* and LB_* documented numeric values.
const LBS_NOINTEGRALHEIGHT: u32 = 0x0100;
const LBS_NOSEL: u32 = 0x0400;
/// Always show the scrollbar even when all items fit (grayed out when unneeded).
const LBS_DISABLENOSCROLL: u32 = 0x1000;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_ADDSTRING: u32 = 0x0180;
/// WM_SETFONT: set the font on a control. wParam = HFONT, lParam = fRedraw.
const WM_SETFONT: u32 = 0x0030;
// VK_ESCAPE
const VK_ESCAPE: usize = 0x1B;
// WA_INACTIVE
const WA_INACTIVE: usize = 0;

// GDI font constants
const FW_NORMAL: i32 = 400;
const ANSI_CHARSET: u32 = 0;
const OUT_DEFAULT_PRECIS: u32 = 0;
const CLIP_DEFAULT_PRECIS: u32 = 0;
/// ClearType anti-aliasing for clean sub-pixel rendering.
const CLEARTYPE_QUALITY: u32 = 5;
const DEFAULT_PITCH: u32 = 0;

const POPUP_WIDTH: i32 = 660;
const POPUP_HEIGHT: i32 = 480;
/// Inner padding between window edge and controls.
const PADDING: i32 = 6;
/// Height of the "Recent activity" title bar at the top.
const TITLE_HEIGHT: i32 = 22;

/// Set by the WndProc when the popup should be closed; read by the main thread.
static POPUP_CLOSE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// HWND of the listbox child stored as `isize` so we can access it from the
/// static WndProc without unsafe global state tricks.
static LISTBOX_HWND: AtomicIsize = AtomicIsize::new(0);
/// HWND of the title static control.
static TITLE_HWND: AtomicIsize = AtomicIsize::new(0);
/// Command ID posted via WM_COMMAND when the user selects a context-menu item.
/// The main thread drains this with [`take_pending_command`].
static PENDING_COMMAND: AtomicU32 = AtomicU32::new(0);

/// The "TaskbarCreated" registered message ID, initialised on first use.
/// When Explorer restarts it broadcasts this to all top-level windows so they
/// can re-register their notification icons.
static S_U_TASKBAR_RESTART: OnceLock<u32> = OnceLock::new();

fn taskbar_restart_msg() -> u32 {
    *S_U_TASKBAR_RESTART.get_or_init(|| unsafe {
        RegisterWindowMessageA(b"TaskbarCreated\0".as_ptr())
    })
}

fn class_name_w() -> Vec<u16> {
    "FindAnythingPopup\0".encode_utf16().collect()
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ACTIVATE => {
            // Low word of wParam is the activation state; 0 = WA_INACTIVE.
            if (wparam & 0xFFFF) == WA_INACTIVE {
                POPUP_CLOSE_REQUESTED.store(true, Ordering::Relaxed);
                ShowWindow(hwnd, SW_HIDE);
            }
            0
        }
        WM_KEYDOWN => {
            if wparam == VK_ESCAPE {
                POPUP_CLOSE_REQUESTED.store(true, Ordering::Relaxed);
                ShowWindow(hwnd, SW_HIDE);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_COMMAND => {
            // HIWORD(wParam) == 0 means a menu item was selected (not a control notification).
            // We store the command ID so the main thread can dispatch it.
            if (wparam >> 16) == 0 {
                let cmd_id = (wparam & 0xFFFF) as u32;
                if cmd_id != 0 {
                    PENDING_COMMAND.store(cmd_id, Ordering::Relaxed);
                }
            }
            0
        }
        WM_SIZE => {
            let title = TITLE_HWND.load(Ordering::Relaxed) as HWND;
            let lb = LISTBOX_HWND.load(Ordering::Relaxed) as HWND;
            if lb != 0 {
                let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetClientRect(hwnd, &mut rc);
                let w = rc.right - rc.left;
                let h = rc.bottom - rc.top;
                if title != 0 {
                    MoveWindow(title, PADDING, PADDING, w - 2 * PADDING, TITLE_HEIGHT, 1);
                }
                let lb_y = PADDING + TITLE_HEIGHT + PADDING;
                MoveWindow(lb, PADDING, lb_y, w - 2 * PADDING, h - lb_y - PADDING, 1);
            }
            0
        }
        _ if msg == taskbar_restart_msg() => {
            // Explorer restarted — signal the main thread to re-register the
            // GUID-based tray icon (tray-icon will re-register its uID-based
            // one; we follow up by replacing it with our stable GUID version).
            crate::guid_icon::NEED_REREGISTER.store(true, Ordering::Relaxed);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Register the popup window class.  Must be called once before creating any
/// [`Popup`].  Safe to call multiple times (ignores ERROR_CLASS_ALREADY_EXISTS).
pub fn register_class() -> Result<()> {
    let class_name = class_name_w();
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        // CS_DROPSHADOW gives the popup a subtle Windows-native drop shadow.
        style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: 0,
        hCursor: 0,
        // COLOR_WINDOW + 1 = 6: use system window background colour.
        hbrBackground: 6,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: 0,
    };

    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        let err = unsafe { GetLastError() };
        const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
        if err != ERROR_CLASS_ALREADY_EXISTS {
            anyhow::bail!("RegisterClassExW failed: {err}");
        }
    }
    Ok(())
}

/// The popup window and its listbox child.
pub struct Popup {
    hwnd: HWND,
    listbox: HWND,
}

impl Popup {
    /// Create the popup window (hidden).  Call [`Popup::show`] to display it.
    pub fn create() -> Result<Self> {
        let class_name = class_name_w();
        let title: Vec<u16> = "Find Anything\0".encode_utf16().collect();
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST, // hide from Alt-Tab; always on top
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
                0, 0, POPUP_WIDTH, POPUP_HEIGHT,
                0, // no parent
                0, // no menu
                hinstance,
                std::ptr::null(),
            )
        };
        if hwnd == 0 {
            anyhow::bail!("CreateWindowExW failed for popup");
        }

        // Allow the "TaskbarCreated" broadcast through UIPI so elevated apps
        // receive it and can re-register their notification icons after
        // Explorer restarts.
        unsafe {
            ChangeWindowMessageFilterEx(
                hwnd,
                taskbar_restart_msg(),
                MSGFLT_ALLOW,
                std::ptr::null_mut(),
            );
        }

        // "Recent activity" title label.
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let title_text: Vec<u16> = "Recent activity\0".encode_utf16().collect();
        let title_ctrl = unsafe {
            CreateWindowExW(
                0,
                static_class.as_ptr(),
                title_text.as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_CENTER | SS_NOPREFIX,
                PADDING, PADDING, POPUP_WIDTH - 2 * PADDING, TITLE_HEIGHT,
                hwnd,
                0,
                hinstance,
                std::ptr::null(),
            )
        };
        TITLE_HWND.store(title_ctrl as isize, Ordering::Relaxed);

        let lb_class: Vec<u16> = "LISTBOX\0".encode_utf16().collect();
        let lb_y = PADDING + TITLE_HEIGHT + PADDING;
        let listbox = unsafe {
            CreateWindowExW(
                0,
                lb_class.as_ptr(),
                std::ptr::null(),
                // LBS_DISABLENOSCROLL: vertical scrollbar always visible (greyed when unneeded)
                // so users always know they can scroll through recent files.
                WS_CHILD | WS_VISIBLE | WS_VSCROLL | LBS_NOSEL | LBS_NOINTEGRALHEIGHT | LBS_DISABLENOSCROLL,
                PADDING, lb_y, POPUP_WIDTH - 2 * PADDING, POPUP_HEIGHT - lb_y - PADDING,
                hwnd,
                0,
                hinstance,
                std::ptr::null(),
            )
        };
        if listbox == 0 {
            unsafe { DestroyWindow(hwnd); }
            anyhow::bail!("CreateWindowExW failed for listbox");
        }

        LISTBOX_HWND.store(listbox as isize, Ordering::Relaxed);

        // Apply Segoe UI 10pt with ClearType so the list reads cleanly.
        // -13 logical units ≈ 10pt at 96 DPI.  The HFONT is intentionally
        // leaked — it lives for the process lifetime alongside the listbox.
        let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
        let hfont = unsafe {
            CreateFontW(
                -13, 0, 0, 0,
                FW_NORMAL, 0, 0, 0,
                ANSI_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY, DEFAULT_PITCH,
                face.as_ptr(),
            )
        };
        if hfont != 0 {
            unsafe {
                SendMessageW(listbox, WM_SETFONT, hfont as WPARAM, 1);
                if title_ctrl != 0 {
                    SendMessageW(title_ctrl, WM_SETFONT, hfont as WPARAM, 1);
                }
            }
        }

        Ok(Self { hwnd, listbox })
    }

    /// Show the popup positioned just above the taskbar at the bottom-right of
    /// the work area.
    pub fn show(&self) {
        let mut work_area = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                &mut work_area as *mut RECT as *mut std::ffi::c_void,
                0,
            );
        }

        let x = work_area.right - POPUP_WIDTH;
        let y = work_area.bottom - POPUP_HEIGHT;

        unsafe {
            SetWindowPos(self.hwnd, 0, x, y, POPUP_WIDTH, POPUP_HEIGHT, SWP_NOZORDER);
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
    }

    /// Hide the popup.
    pub fn hide(&self) {
        unsafe { ShowWindow(self.hwnd, SW_HIDE); }
    }

    /// The underlying Win32 window handle (used as menu owner for right-click).
    pub fn hwnd(&self) -> isize { self.hwnd }

    /// True if the popup window is currently visible.
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) != 0 }
    }

    /// Repopulate the listbox with the given recently-indexed files.
    /// Shows "Connecting…" when `files` is empty.
    pub fn update_files(&self, files: &[RecentFile]) {
        unsafe { SendMessageW(self.listbox, LB_RESETCONTENT, 0, 0) };

        if files.is_empty() {
            add_row(self.listbox, "Connecting\u{2026}");
            return;
        }

        for file in files {
            add_row(self.listbox, &format_row(file));
        }
    }
}

/// Check whether the WndProc has requested the popup to close and clear the
/// flag.  Call from the main thread's `about_to_wait` handler.
pub fn take_close_request() -> bool {
    POPUP_CLOSE_REQUESTED.swap(false, Ordering::Relaxed)
}

/// Drain any context-menu command posted by the WndProc via WM_COMMAND.
/// Returns `Some(cmd_id)` once per selection; the `cmd_id` matches the muda
/// internal ID, which equals `menu_item.id().0.parse::<u32>()`.
pub fn take_pending_command() -> Option<u32> {
    let id = PENDING_COMMAND.swap(0, Ordering::Relaxed);
    if id != 0 { Some(id) } else { None }
}

fn add_row(listbox: HWND, text: &str) {
    let wide: Vec<u16> = format!("{text}\0").encode_utf16().collect();
    unsafe { SendMessageW(listbox, LB_ADDSTRING, 0, wide.as_ptr() as LPARAM) };
}

/// Format a listbox row: `[source]  full/path/to/file`
fn format_row(file: &RecentFile) -> String {
    format!("[{}]  {}", file.source, file.path)
}
