//! Win32 borderless popup window that lists the most recently indexed files.
//!
//! The popup appears above the taskbar at the bottom-right of the work area
//! when the user left-clicks the tray icon.  It dismisses automatically when
//! it loses activation (user clicks elsewhere) or when the user presses Escape.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use anyhow::Result;
use find_common::api::RecentFile;

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, IsWindowVisible,
    MoveWindow, RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowPos,
    ShowWindow, SystemParametersInfoW, WNDCLASSEXW,
    CS_HREDRAW, CS_VREDRAW,
    SW_HIDE, SW_SHOW,
    SPI_GETWORKAREA,
    WM_ACTIVATE, WM_KEYDOWN, WM_SIZE,
    WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_POPUP, WS_VISIBLE, WS_VSCROLL,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    SWP_NOZORDER,
};

// LBS_* and LB_* are in Win32_UI_WindowsAndMessaging but not always re-exported
// by name, so use their documented numeric values.
const LBS_NOINTEGRALHEIGHT: u32 = 0x0100;
const LBS_NOSEL: u32 = 0x0400;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_ADDSTRING: u32 = 0x0180;
// VK_ESCAPE
const VK_ESCAPE: usize = 0x1B;
// WA_INACTIVE
const WA_INACTIVE: usize = 0;

const POPUP_WIDTH: i32 = 440;
const POPUP_HEIGHT: i32 = 420;

/// Set by the WndProc when the popup should be closed; read by the main thread.
static POPUP_CLOSE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// HWND of the listbox child stored as `isize` so we can access it from the
/// static WndProc without unsafe global state tricks.
static LISTBOX_HWND: AtomicIsize = AtomicIsize::new(0);

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
        WM_SIZE => {
            let lb = LISTBOX_HWND.load(Ordering::Relaxed) as HWND;
            if lb != 0 {
                let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetClientRect(hwnd, &mut rc);
                MoveWindow(lb, 0, 0, rc.right - rc.left, rc.bottom - rc.top, 1);
            }
            0
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
        style: CS_HREDRAW | CS_VREDRAW,
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

        let lb_class: Vec<u16> = "LISTBOX\0".encode_utf16().collect();
        let listbox = unsafe {
            CreateWindowExW(
                0,
                lb_class.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_VSCROLL | LBS_NOSEL | LBS_NOINTEGRALHEIGHT,
                0, 0, POPUP_WIDTH, POPUP_HEIGHT,
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

fn add_row(listbox: HWND, text: &str) {
    let wide: Vec<u16> = format!("{text}\0").encode_utf16().collect();
    unsafe { SendMessageW(listbox, LB_ADDSTRING, 0, wide.as_ptr() as LPARAM) };
}

/// Format a listbox row: `[source]  basename   (parent)`
fn format_row(file: &RecentFile) -> String {
    use std::path::Path;
    let p = Path::new(&file.path);
    let basename = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&file.path);
    let parent = p
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if parent.is_empty() {
        format!("[{}]  {}", file.source, basename)
    } else {
        format!("[{}]  {}   ({})", file.source, basename, parent)
    }
}
