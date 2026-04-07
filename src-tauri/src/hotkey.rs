//! Detect Shift hold/release using GetAsyncKeyState polling.
//! WH_KEYBOARD_LL hooks miss the release event when the overlay captures focus,
//! so we poll the actual hardware state every 50ms instead.

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    const VK_SHIFT: i32 = 0x10;
    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn start_shift_poll<F: Fn(bool) + Send + Sync + 'static>(callback: F) {
        RUNNING.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            let mut was_pressed = false;

            while RUNNING.load(Ordering::SeqCst) {
                let pressed = unsafe { GetAsyncKeyState(VK_SHIFT) & (0x8000u16 as i16) != 0 };

                if pressed != was_pressed {
                    callback(pressed);
                    was_pressed = pressed;
                }

                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    }
}

#[cfg(windows)]
pub use imp::start_shift_poll;

#[cfg(not(windows))]
pub fn start_shift_poll<F: Fn(bool) + Send + Sync + 'static>(_callback: F) {}
