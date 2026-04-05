//! Focus restoration helpers.
//!
//! The LoL client forcibly grabs the foreground window when ready check pops
//! and during champ select. Because our accept/pick/ban actions go through the
//! LCU HTTP API (not a simulated click), we have a short window where we can
//! capture whatever window the user had focused, then — after LoL has done its
//! focus-steal — put the user's window back on top using the classic
//! `AttachThreadInput` + `SetForegroundWindow` trick, which bypasses the
//! Windows `ForegroundLockTimeout` restriction.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };

    /// Returns the HWND of the currently foreground window, encoded as `isize`
    /// so it can cross `Send` boundaries (raw HWND is `*mut c_void`, not Send).
    pub fn capture_foreground() -> Option<isize> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                None
            } else {
                Some(hwnd as isize)
            }
        }
    }

    /// Force `hwnd` back to the foreground, defeating Windows focus-stealing
    /// protections by temporarily attaching this thread's input queue to the
    /// thread of whatever window currently holds foreground.
    pub fn restore_foreground(hwnd_raw: isize) {
        if hwnd_raw == 0 {
            return;
        }
        unsafe {
            let target: HWND = hwnd_raw as HWND;

            let fg = GetForegroundWindow();
            let fg_thread = if fg.is_null() {
                0
            } else {
                GetWindowThreadProcessId(fg, std::ptr::null_mut())
            };
            let our_thread = GetCurrentThreadId();

            // Attach input queues so SetForegroundWindow sees us as "related"
            // to the current foreground thread — this is the magic that lets
            // the call actually switch focus instead of just flashing the
            // taskbar button.
            let mut attached = false;
            if fg_thread != 0 && fg_thread != our_thread {
                attached = AttachThreadInput(our_thread, fg_thread, 1) != 0;
            }

            BringWindowToTop(target);
            SetForegroundWindow(target);
            SetFocus(target);

            if attached {
                AttachThreadInput(our_thread, fg_thread, 0);
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{capture_foreground, restore_foreground};

#[cfg(not(windows))]
pub fn capture_foreground() -> Option<isize> {
    None
}

#[cfg(not(windows))]
pub fn restore_foreground(_hwnd: isize) {}
