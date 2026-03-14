//! Windows notification-area icon registration using a stable NIF_GUID.
//!
//! tray-icon 0.21 registers the Shell notification icon with `uID`, which
//! Windows ties to the executable path + uID pair in its settings store.
//! The pinned/hidden preference is therefore lost whenever the installer
//! force-kills find-tray.exe (breaking the pairing) or if Windows decides the
//! entry is stale.
//!
//! Using NIF_GUID instead gives Windows a fixed, version-independent identity
//! for the icon.  The preference survives reinstalls, force-kills, and Explorer
//! restarts (after we re-register on TaskbarCreated).

use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_GUID, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIcon, HICON};

/// Must match the `WM_USER_TRAYICON` constant in tray-icon's Windows backend
/// (`platform_impl/windows/mod.rs`).  If tray-icon ever changes this value,
/// click events will stop working and this constant must be updated too.
const WM_USER_TRAYICON: u32 = 6002;

/// Stable GUID for the Find Anything tray icon.
///
/// Must **never** change between releases — Windows uses it to match the
/// notification-area entry in its settings store across reinstalls.
///
/// {8A3F5D2C-1B4E-4F7A-9C8D-0E6B2A5F3D91}  (same as the Inno Setup AppId)
const TRAY_GUID: GUID = GUID {
    data1: 0x8A3F5D2C,
    data2: 0x1B4E,
    data3: 0x4F7A,
    data4: [0x9C, 0x8D, 0x0E, 0x6B, 0x2A, 0x5F, 0x3D, 0x91],
};

/// Set by the popup wnd_proc when a `TaskbarCreated` broadcast is received.
/// Cleared by [`reregister_if_needed`] in `about_to_wait`.
pub static NEED_REREGISTER: AtomicBool = AtomicBool::new(false);

/// Build an `HICON` from raw ICO file bytes.
///
/// Uses the same `CreateIcon` approach as tray-icon's Windows backend so the
/// icon is rendered identically.  The returned handle is intentionally
/// process-lifetime; do not call `DestroyIcon` on it.
pub fn load_hicon(bytes: &[u8]) -> Result<HICON> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Ico)
        .context("decoding ICO file")?;
    let img = img.into_rgba8();
    let (w, h) = img.dimensions();
    let mut rgba = img.into_raw();

    let pixel_count = rgba.len() / 4;
    let mut and_mask = Vec::with_capacity(pixel_count);
    for chunk in rgba.chunks_mut(4) {
        // Invert the alpha channel into the AND mask: 0 = opaque, non-zero = transparent.
        and_mask.push(chunk[3].wrapping_sub(u8::MAX));
        chunk.swap(0, 2); // RGBA → BGRA
    }

    let handle = unsafe {
        CreateIcon(
            0isize,
            w as i32,
            h as i32,
            1,
            32,
            and_mask.as_ptr(),
            rgba.as_ptr(),
        )
    };
    if handle == 0 {
        anyhow::bail!("CreateIcon failed: {}", std::io::Error::last_os_error());
    }
    Ok(handle)
}

/// Delete the uID-based icon that tray-icon registered (uID = 1, because
/// tray-icon's internal COUNTER starts at 1 and we create exactly one
/// `TrayIcon`), then re-add it under our stable GUID.
///
/// # Safety
/// `hwnd` must be the valid hidden window returned by `tray_icon.hwnd()`.
pub unsafe fn reregister_with_guid(hwnd: HWND, hicon: HICON, tooltip: &str) {
    // Delete the uID=1 icon that tray-icon added via NIM_ADD.
    let mut nid_del: NOTIFYICONDATAW = std::mem::zeroed();
    nid_del.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid_del.hWnd = hwnd;
    nid_del.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &mut nid_del);

    // Re-add with a stable GUID so Windows preserves the pinned state.
    let mut sz_tip = [0u16; 128];
    for (i, c) in tooltip.encode_utf16().take(127).enumerate() {
        sz_tip[i] = c;
    }
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uFlags = NIF_GUID | NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_USER_TRAYICON;
    nid.hIcon = hicon;
    nid.szTip = sz_tip;
    nid.guidItem = TRAY_GUID;
    Shell_NotifyIconW(NIM_ADD, &mut nid);
}

/// Update the icon for the GUID-registered notification icon.
///
/// # Safety
/// `hwnd` must be the valid hidden window returned by `tray_icon.hwnd()`.
pub unsafe fn update_icon(hwnd: HWND, hicon: HICON) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uFlags = NIF_GUID | NIF_ICON;
    nid.hIcon = hicon;
    nid.guidItem = TRAY_GUID;
    Shell_NotifyIconW(NIM_MODIFY, &mut nid);
}

/// Update the tooltip for the GUID-registered notification icon.
///
/// # Safety
/// `hwnd` must be the valid hidden window returned by `tray_icon.hwnd()`.
pub unsafe fn update_tooltip(hwnd: HWND, tooltip: &str) {
    let mut sz_tip = [0u16; 128];
    for (i, c) in tooltip.encode_utf16().take(127).enumerate() {
        sz_tip[i] = c;
    }
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uFlags = NIF_GUID | NIF_TIP;
    nid.szTip = sz_tip;
    nid.guidItem = TRAY_GUID;
    Shell_NotifyIconW(NIM_MODIFY, &mut nid);
}
