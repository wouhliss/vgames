//! The window every test app draws into, and its message pump.

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SW_SHOW, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{PCWSTR, w};

use crate::{HEIGHT, WIDTH};

#[allow(unsafe_code)]
extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwards the arguments Windows passed to this window procedure unchanged.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Creates and shows a `WIDTH`×`HEIGHT` client-area window titled `title`.
#[allow(unsafe_code)]
pub fn create_window(title: PCWSTR) -> windows::core::Result<HWND> {
    // SAFETY: plain Win32 calls with valid, fully initialized structures; the class
    // name and title are static wide strings that outlive the window.
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("vgames-testapp");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let mut rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: i32::try_from(WIDTH).unwrap_or(640),
            bottom: i32::try_from(HEIGHT).unwrap_or(480),
        };
        AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false)?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            title,
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        Ok(hwnd)
    }
}

/// Handles pending window messages without blocking.
#[allow(unsafe_code)]
pub fn pump_messages() {
    // SAFETY: the standard non-blocking message loop over a stack-allocated MSG.
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
