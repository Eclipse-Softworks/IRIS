//! Cross-platform system clipboard helpers for `HostMode::NativeWindow`.
//!
//! `HostMode::Headless` keeps clipboard state isolated inside `UiHost` so tests
//! never mutate the developer's OS clipboard.

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd_new_owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
        fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalFree(mem: *mut c_void) -> *mut c_void;
        fn GlobalLock(mem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(mem: *mut c_void) -> i32;
    }

    struct ClipboardGuard;

    impl ClipboardGuard {
        fn open() -> Result<Self, String> {
            // Retry briefly in case another process holds the clipboard momentarily.
            for attempt in 0..5 {
                // SAFETY: Null HWND opens the clipboard associated with the current task.
                if unsafe { OpenClipboard(ptr::null_mut()) } != 0 {
                    return Ok(Self);
                }
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
            Err("OpenClipboard failed".to_owned())
        }
    }

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            // SAFETY: Paired with a successful `OpenClipboard` on the same thread.
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    struct LockGuard {
        mem: *mut c_void,
        ptr: *mut c_void,
    }

    impl LockGuard {
        fn lock(mem: *mut c_void) -> Option<Self> {
            if mem.is_null() {
                return None;
            }
            // SAFETY: `mem` is a valid global memory handle from `GetClipboardData` or `GlobalAlloc`.
            let ptr = unsafe { GlobalLock(mem) };
            if ptr.is_null() {
                None
            } else {
                Some(Self { mem, ptr })
            }
        }
    }

    impl Drop for LockGuard {
        fn drop(&mut self) {
            // SAFETY: Paired with a successful `GlobalLock` on `self.mem`.
            unsafe {
                let _ = GlobalUnlock(self.mem);
            }
        }
    }

    pub fn get_text() -> Result<String, String> {
        let _guard = ClipboardGuard::open()?;
        // SAFETY: Clipboard is open via `_guard`; `CF_UNICODETEXT` returns a NUL-terminated UTF-16 handle or null.
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        let Some(lock) = LockGuard::lock(handle) else {
            return Ok(String::new());
        };
        let wide_ptr = lock.ptr as *const u16;
        let mut len = 0usize;
        // SAFETY: `CF_UNICODETEXT` guarantees NUL termination; bound scan at 4 Mi chars defensively.
        while len < 4 * 1024 * 1024 {
            // SAFETY: `wide_ptr.add(len)` is within the locked clipboard NUL-terminated UTF-16 buffer.
            let ch = unsafe { *wide_ptr.add(len) };
            if ch == 0 {
                break;
            }
            len += 1;
        }
        // SAFETY: `wide_ptr[..len]` is valid locked UTF-16 units for the lifetime of `lock`.
        let units = unsafe { std::slice::from_raw_parts(wide_ptr, len) };
        Ok(String::from_utf16_lossy(units))
    }

    pub fn set_text(text: &str) -> Result<(), String> {
        let _guard = ClipboardGuard::open()?;
        // SAFETY: Clipboard is open via `_guard`.
        if unsafe { EmptyClipboard() } == 0 {
            return Err("EmptyClipboard failed".to_owned());
        }
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * std::mem::size_of::<u16>();
        // SAFETY: Allocates `GMEM_MOVEABLE` memory of `bytes` size for `SetClipboardData`.
        let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if hmem.is_null() {
            return Err("GlobalAlloc failed".to_owned());
        }
        {
            let Some(lock) = LockGuard::lock(hmem) else {
                // SAFETY: `hmem` has not been handed to `SetClipboardData`, so we still own it.
                unsafe {
                    let _ = GlobalFree(hmem);
                }
                return Err("GlobalLock failed".to_owned());
            };
            // SAFETY: `lock.ptr` points to `bytes` writable bytes (`wide.len()` u16 units).
            unsafe {
                ptr::copy_nonoverlapping(wide.as_ptr(), lock.ptr as *mut u16, wide.len());
            }
        }
        // SAFETY: `hmem` is unlocked and valid; ownership transfers to Windows if `SetClipboardData` returns non-null.
        let res = unsafe { SetClipboardData(CF_UNICODETEXT, hmem) };
        if res.is_null() {
            // SAFETY: `SetClipboardData` failed, so caller retains ownership of `hmem`.
            unsafe {
                let _ = GlobalFree(hmem);
            }
            return Err("SetClipboardData failed".to_owned());
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io::Write;
    use std::process::{Command, Stdio};

    pub fn get_text() -> Result<String, String> {
        let out = Command::new("pbpaste")
            .output()
            .map_err(|e| format!("pbpaste failed: {e}"))?;
        if !out.status.success() {
            return Err("pbpaste returned non-zero status".to_owned());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn set_text(text: &str) -> Result<(), String> {
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("pbcopy failed: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let status = child
            .wait()
            .map_err(|e| format!("pbcopy wait failed: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("pbcopy returned non-zero status".to_owned())
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::io::Write;
    use std::process::{Command, Stdio};

    pub fn get_text() -> Result<String, String> {
        for (bin, args) in [
            ("wl-paste", &["--no-newline"][..]),
            ("xclip", &["-o", "-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--output"][..]),
        ] {
            if let Ok(out) = Command::new(bin).args(args).output() {
                if out.status.success() {
                    return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
                }
            }
        }
        Err("no clipboard utility (wl-paste/xclip/xsel) available".to_owned())
    }

    pub fn set_text(text: &str) -> Result<(), String> {
        for (bin, args) in [
            ("wl-copy", &[][..]),
            ("xclip", &["-i", "-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--input"][..]),
        ] {
            if let Ok(mut child) = Command::new(bin).args(args).stdin(Stdio::piped()).spawn() {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if let Ok(status) = child.wait() {
                    if status.success() {
                        return Ok(());
                    }
                }
            }
        }
        Err("no clipboard utility (wl-copy/xclip/xsel) available".to_owned())
    }
}

pub fn get_system_text() -> Result<String, String> {
    imp::get_text()
}

pub fn set_system_text(text: &str) -> Result<(), String> {
    imp::set_text(text)
}
