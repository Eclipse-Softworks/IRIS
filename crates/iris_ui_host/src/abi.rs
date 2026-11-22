use std::cell::RefCell;
use std::convert::TryFrom;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;

use crate::camera::SimTargetSpec;
use crate::protocol::{decode_event_record, CursorKind, HostMode};
use crate::window::UiHost;

const HOST_ABI_VERSION: i64 = 0x0001_0000;

thread_local! {
    static GLOBAL_LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_global_error(msg: impl Into<String>) {
    let text = msg.into();
    GLOBAL_LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = text;
    });
}

unsafe fn read_c_str(ptr: *const c_char, fallback: &str) -> String {
    if ptr.is_null() {
        return fallback.to_owned();
    }
    // SAFETY: Caller passes a valid NUL-terminated C string from IRIS FFI.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_string_lossy().into_owned()
}

unsafe fn write_out_i32(base: *mut u8, word_index: usize, value: i32) {
    // SAFETY: Caller verifies `out_cap` covers `(word_index + 1) * 4` bytes.
    let ptr = unsafe { base.add(word_index * 4) };
    unsafe {
        std::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), ptr, 4);
    }
}

unsafe fn write_out_f32(base: *mut u8, word_index: usize, value: f32) {
    // SAFETY: Caller verifies `out_cap` covers `(word_index + 1) * 4` bytes.
    let ptr = unsafe { base.add(word_index * 4) };
    unsafe {
        std::ptr::copy_nonoverlapping(value.to_bits().to_le_bytes().as_ptr(), ptr, 4);
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_version() -> i64 {
    HOST_ABI_VERSION
}

/// Create a new native desktop (`mode = 0`) or headless (`mode = 1`) host.
///
/// # Safety
/// `title_ptr` must be either null or point to a valid NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn iris_ui_host_create(
    mode_raw: i64,
    title_ptr: *const c_char,
    width: i64,
    height: i64,
    scale_milli: i64,
) -> i64 {
    let Ok(mode) = HostMode::try_from(mode_raw) else {
        set_global_error(format!("invalid host mode {mode_raw}"));
        return 0;
    };
    // SAFETY: `title_ptr` is either null or a valid C string passed by IRIS FFI.
    let title = unsafe { read_c_str(title_ptr, "IRIS Studio") };
    let w = width.clamp(64, 8192) as u32;
    let h = height.clamp(64, 8192) as u32;
    let scale = scale_milli.max(0) as u32;

    match UiHost::create(mode, &title, w, h, scale) {
        Ok(host) => Box::into_raw(Box::new(host)) as i64,
        Err(err) => {
            set_global_error(err);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_host_destroy(host_ptr: i64) -> i64 {
    if host_ptr == 0 {
        return 0;
    }
    // SAFETY: `host_ptr` came from `Box::into_raw` in `iris_ui_host_create`.
    let _ = unsafe { Box::from_raw(host_ptr as *mut UiHost) };
    0
}

#[no_mangle]
pub extern "C" fn iris_ui_is_open(host_ptr: i64) -> i64 {
    if host_ptr == 0 {
        return 0;
    }
    // SAFETY: Valid `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    i64::from(host.is_open())
}

#[no_mangle]
pub extern "C" fn iris_ui_get_metrics(host_ptr: i64, out_cell: i64, out_cap: i64) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap < 16 {
        set_global_error("invalid host handle or output buffer in iris_ui_get_metrics");
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and `out_cell` buffer of at least 16 bytes.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    let m = host.metrics();
    let dst = out_cell as *mut u8;
    // SAFETY: Bounds `out_cap >= 16` verified above.
    unsafe {
        write_out_i32(dst, 0, m.logical_width as i32);
        write_out_i32(dst, 1, m.logical_height as i32);
        write_out_i32(dst, 2, m.scale_milli as i32);
        write_out_i32(dst, 3, i32::from(m.is_open));
    }
    0
}

#[no_mangle]
pub extern "C" fn iris_ui_measure_text(
    host_ptr: i64,
    req_cell: i64,
    req_len: i64,
    out_cell: i64,
    out_cap: i64,
) -> i64 {
    if host_ptr == 0 || req_cell == 0 || req_len <= 0 || out_cell == 0 || out_cap < 12 {
        set_global_error("invalid arguments to iris_ui_measure_text");
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and input/output slices of checked lengths.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let req_bytes = unsafe { std::slice::from_raw_parts(req_cell as *const u8, req_len as usize) };
    match host.measure_text(req_bytes) {
        Ok(m) => {
            let dst = out_cell as *mut u8;
            // SAFETY: Bounds `out_cap >= 12` verified above.
            unsafe {
                write_out_f32(dst, 0, m.width);
                write_out_f32(dst, 1, m.height);
                write_out_i32(dst, 2, m.lines);
            }
            0
        }
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_submit_frame(host_ptr: i64, cmd_cell: i64, cmd_len: i64) -> i64 {
    if host_ptr == 0 || cmd_cell == 0 || cmd_len < 16 {
        set_global_error("invalid command buffer in iris_ui_submit_frame");
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and readable `cmd_cell[..cmd_len]`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let bytes = unsafe { std::slice::from_raw_parts(cmd_cell as *const u8, cmd_len as usize) };
    match host.submit_frame(bytes) {
        Ok(()) => 0,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_poll_events(
    host_ptr: i64,
    timeout_ms: i64,
    out_cell: i64,
    out_cap: i64,
) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap < 0 {
        set_global_error("invalid buffer in iris_ui_poll_events");
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and writable `out_cell[..out_cap]`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let buf = unsafe { std::slice::from_raw_parts_mut(out_cell as *mut u8, out_cap as usize) };
    match host.poll_events(timeout_ms, buf) {
        Ok(count) => count as i64,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_inject_event(host_ptr: i64, ev_cell: i64, ev_len: i64) -> i64 {
    if host_ptr == 0 || ev_cell == 0 || ev_len < 64 {
        set_global_error("invalid event packet in iris_ui_inject_event");
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and readable `ev_cell[..ev_len]`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let bytes = unsafe { std::slice::from_raw_parts(ev_cell as *const u8, ev_len as usize) };
    match decode_event_record(bytes) {
        Ok(ev) => {
            host.inject_event(ev);
            0
        }
        Err(err) => {
            let msg = err.to_string();
            host.set_last_error(msg.clone());
            set_global_error(msg);
            -2
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_framebuffer_dims(host_ptr: i64, out_cell: i64, out_cap: i64) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap < 8 {
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and `out_cap >= 8`.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    let (w, h) = host.framebuffer_dims();
    let dst = out_cell as *mut u8;
    // SAFETY: `out_cap >= 8` verified above.
    unsafe {
        write_out_i32(dst, 0, w as i32);
        write_out_i32(dst, 1, h as i32);
    }
    0
}

#[no_mangle]
pub extern "C" fn iris_ui_copy_rgba(host_ptr: i64, out_cell: i64, out_cap: i64) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap < 0 {
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and writable `out_cell[..out_cap]`.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    let src = host.rgba_bytes();
    if (out_cap as usize) < src.len() {
        return -(src.len() as i64);
    }
    // SAFETY: Destination verified large enough for `src.len()`.
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), out_cell as *mut u8, src.len());
    }
    src.len() as i64
}

#[no_mangle]
pub extern "C" fn iris_ui_sample_pixel(host_ptr: i64, x_px: i64, y_px: i64) -> i64 {
    if host_ptr == 0 || x_px < 0 || y_px < 0 {
        return 0;
    }
    // SAFETY: Valid `UiHost` handle.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    match host.sample_pixel(x_px as u32, y_px as u32) {
        Some(c) => (c.to_packed_u32() as i64) & 0xFFFF_FFFF,
        None => 0,
    }
}

/// Save the current rendered framebuffer as a PNG image at `path_ptr`.
///
/// # Safety
/// `path_ptr` must be either null or point to a valid NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn iris_ui_save_png(host_ptr: i64, path_ptr: *const c_char) -> i64 {
    if host_ptr == 0 || path_ptr.is_null() {
        set_global_error("invalid host or path in iris_ui_save_png");
        return -1;
    }
    // SAFETY: Valid `UiHost` handle and C string path.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let path_str = unsafe { read_c_str(path_ptr, "") };
    if path_str.is_empty() {
        set_global_error("empty PNG output path");
        return -1;
    }
    match host.save_png(Path::new(&path_str)) {
        Ok(()) => 0,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_last_error(host_ptr: i64, out_cell: i64, out_cap: i64) -> i64 {
    if out_cell == 0 || out_cap <= 0 {
        return 0;
    }
    let msg = if host_ptr != 0 {
        // SAFETY: Valid `UiHost` handle.
        let host = unsafe { &*(host_ptr as *const UiHost) };
        if !host.last_error().is_empty() {
            host.last_error().to_owned()
        } else {
            GLOBAL_LAST_ERROR.with(|slot| slot.borrow().clone())
        }
    } else {
        GLOBAL_LAST_ERROR.with(|slot| slot.borrow().clone())
    };
    let bytes = msg.as_bytes();
    let max_copy = (out_cap as usize).saturating_sub(1);
    let n = bytes.len().min(max_copy);
    // SAFETY: `out_cell` has at least `out_cap` bytes and `n + 1 <= out_cap`.
    unsafe {
        let dst = out_cell as *mut u8;
        if n > 0 {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, n);
        }
        *dst.add(n) = 0;
    }
    n as i64
}

/// Create an additional window in the host. Returns its 1-based window id.
///
/// # Safety
/// `title_ptr` must be null or a valid NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn iris_ui_window_create(
    host_ptr: i64,
    title_ptr: *const c_char,
    width: i64,
    height: i64,
) -> i64 {
    if host_ptr == 0 {
        set_global_error("invalid host handle in iris_ui_window_create");
        return 0;
    }
    // SAFETY: `title_ptr` is null or a valid C string from IRIS FFI.
    let title = unsafe { read_c_str(title_ptr, "IRIS Studio") };
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    match host.create_window(&title, width.max(64) as u32, height.max(64) as u32) {
        Ok(id) => id,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            0
        }
    }
}

/// Close one window. The host stays alive while another window is open.
#[no_mangle]
pub extern "C" fn iris_ui_window_close(host_ptr: i64, window_id: i64) -> i64 {
    if host_ptr == 0 {
        set_global_error("invalid host handle in iris_ui_window_close");
        return -1;
    }
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    match host.close_window(window_id) {
        Ok(()) => 0,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_window_is_open(host_ptr: i64, window_id: i64) -> i64 {
    if host_ptr == 0 {
        return 0;
    }
    let host = unsafe { &*(host_ptr as *const UiHost) };
    i64::from(host.window_is_open(window_id))
}

#[no_mangle]
pub extern "C" fn iris_ui_window_get_metrics(
    host_ptr: i64,
    window_id: i64,
    out_cell: i64,
    out_cap: i64,
) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap < 16 {
        set_global_error("invalid handle or buffer in iris_ui_window_get_metrics");
        return -1;
    }
    let host = unsafe { &*(host_ptr as *const UiHost) };
    let m = host.metrics_for_window(window_id);
    let dst = out_cell as *mut u8;
    unsafe {
        write_out_i32(dst, 0, m.logical_width as i32);
        write_out_i32(dst, 1, m.logical_height as i32);
        write_out_i32(dst, 2, m.scale_milli as i32);
        write_out_i32(dst, 3, i32::from(m.is_open));
    }
    0
}

#[no_mangle]
pub extern "C" fn iris_ui_window_submit_frame(
    host_ptr: i64,
    window_id: i64,
    cmd_cell: i64,
    cmd_len: i64,
) -> i64 {
    if host_ptr == 0 || cmd_cell == 0 || cmd_len < 16 {
        set_global_error("invalid command buffer in iris_ui_window_submit_frame");
        return -1;
    }
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let bytes = unsafe { std::slice::from_raw_parts(cmd_cell as *const u8, cmd_len as usize) };
    match host.submit_frame_for_window(window_id, bytes) {
        Ok(()) => 0,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

#[no_mangle]
pub extern "C" fn iris_ui_window_poll_events(
    host_ptr: i64,
    window_id: i64,
    timeout_ms: i64,
    out_cell: i64,
    out_cap: i64,
) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap < 0 {
        set_global_error("invalid buffer in iris_ui_window_poll_events");
        return -1;
    }
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let buf = unsafe { std::slice::from_raw_parts_mut(out_cell as *mut u8, out_cap as usize) };
    match host.poll_events_for_window(window_id, timeout_ms, buf) {
        Ok(count) => count as i64,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

fn write_utf8_out(text: &str, out_cell: i64, out_cap: i64) -> i64 {
    if out_cell == 0 || out_cap <= 0 {
        return 0;
    }
    let bytes = text.as_bytes();
    let max_copy = (out_cap as usize).saturating_sub(1);
    let mut n = bytes.len().min(max_copy);
    while n > 0 && !text.is_char_boundary(n) {
        n -= 1;
    }
    // SAFETY: Caller provides writable `out_cell` of at least `out_cap` bytes and `n + 1 <= out_cap`.
    unsafe {
        let dst = out_cell as *mut u8;
        if n > 0 {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, n);
        }
        *dst.add(n) = 0;
    }
    n as i64
}

fn host_result(host: &mut UiHost, res: Result<(), String>) -> i64 {
    match res {
        Ok(()) => 0,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

/// Elapsed monotonic microseconds since the host session was created.
#[no_mangle]
pub extern "C" fn iris_ui_now_micros(host_ptr: i64) -> i64 {
    if host_ptr == 0 {
        return 0;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    host.elapsed_micros()
}

/// Set the OS pointer cursor shape for `window_id` (`CursorKind` numeric code).
#[no_mangle]
pub extern "C" fn iris_ui_set_cursor(host_ptr: i64, window_id: i64, cursor_code: i64) -> i64 {
    if host_ptr == 0 {
        set_global_error("invalid host handle in iris_ui_set_cursor");
        return -1;
    }
    let Ok(cursor) = CursorKind::try_from(cursor_code) else {
        set_global_error(format!("invalid cursor kind {cursor_code}"));
        return -1;
    };
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let res = host.set_cursor(window_id, cursor);
    host_result(host, res)
}

/// Update the window title string for `window_id`.
///
/// # Safety
/// `title_ptr` must be null or point to a valid NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn iris_ui_set_title(
    host_ptr: i64,
    window_id: i64,
    title_ptr: *const c_char,
) -> i64 {
    if host_ptr == 0 {
        set_global_error("invalid host handle in iris_ui_set_title");
        return -1;
    }
    // SAFETY: `title_ptr` is null or a valid C string passed by IRIS FFI.
    let title = unsafe { read_c_str(title_ptr, "IRIS App") };
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let res = host.set_title(window_id, &title);
    host_result(host, res)
}

/// Set minimum and maximum logical size bounds (`0` disables that constraint).
#[no_mangle]
pub extern "C" fn iris_ui_set_size_constraints(
    host_ptr: i64,
    window_id: i64,
    min_w: i64,
    min_h: i64,
    max_w: i64,
    max_h: i64,
) -> i64 {
    if host_ptr == 0 {
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let res = host.set_size_constraints(
        window_id,
        min_w.max(0) as u32,
        min_h.max(0) as u32,
        max_w.max(0) as u32,
        max_h.max(0) as u32,
    );
    host_result(host, res)
}

#[no_mangle]
pub extern "C" fn iris_ui_set_resizable(host_ptr: i64, window_id: i64, resizable: i64) -> i64 {
    if host_ptr == 0 {
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let res = host.set_resizable(window_id, resizable != 0);
    host_result(host, res)
}

#[no_mangle]
pub extern "C" fn iris_ui_set_fullscreen(host_ptr: i64, window_id: i64, fullscreen: i64) -> i64 {
    if host_ptr == 0 {
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let res = host.set_fullscreen(window_id, fullscreen != 0);
    host_result(host, res)
}

#[no_mangle]
pub extern "C" fn iris_ui_set_maximized(host_ptr: i64, window_id: i64, maximized: i64) -> i64 {
    if host_ptr == 0 {
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let res = host.set_maximized(window_id, maximized != 0);
    host_result(host, res)
}

/// Set the window icon from straight RGBA8 bytes stored in `rgba_cell[..rgba_len]`.
#[no_mangle]
pub extern "C" fn iris_ui_set_icon_rgba(
    host_ptr: i64,
    window_id: i64,
    width: i64,
    height: i64,
    rgba_cell: i64,
    rgba_len: i64,
) -> i64 {
    if host_ptr == 0 || rgba_cell == 0 || width <= 0 || height <= 0 || rgba_len <= 0 {
        set_global_error("invalid icon arguments in iris_ui_set_icon_rgba");
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle and `rgba_cell` covers `rgba_len` readable bytes.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    // SAFETY: `rgba_cell` and `rgba_len` checked non-null and positive above.
    let bytes = unsafe { std::slice::from_raw_parts(rgba_cell as *const u8, rgba_len as usize) };
    let res = host.set_icon_rgba(window_id, width as u32, height as u32, bytes);
    host_result(host, res)
}

/// Read UTF-8 text from the clipboard (isolated in Headless mode, OS clipboard in Desktop mode).
#[no_mangle]
pub extern "C" fn iris_ui_clipboard_get(host_ptr: i64, out_cell: i64, out_cap: i64) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap <= 0 {
        return 0;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let text = host.clipboard_get();
    write_utf8_out(&text, out_cell, out_cap)
}

/// Write UTF-8 text to the clipboard (isolated in Headless mode, OS clipboard in Desktop mode).
///
/// # Safety
/// `text_ptr` must be null or point to a valid NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn iris_ui_clipboard_set(host_ptr: i64, text_ptr: *const c_char) -> i64 {
    if host_ptr == 0 {
        return -1;
    }
    // SAFETY: `text_ptr` is null or a valid C string passed by IRIS FFI.
    let text = unsafe { read_c_str(text_ptr, "") };
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let res = host.clipboard_set(&text);
    host_result(host, res)
}

/// Pop the next dropped file path received by the window, returning bytes written (`0` when empty).
#[no_mangle]
pub extern "C" fn iris_ui_take_dropped_file(host_ptr: i64, out_cell: i64, out_cap: i64) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap <= 0 {
        return 0;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    match host.take_dropped_file() {
        Some(path) => write_utf8_out(&path, out_cell, out_cap),
        None => 0,
    }
}

/// Open a live laptop camera (`synthetic_only == 0`) or synthetic tactical camera (`!= 0`).
/// Returns positive 1-based `cam_id`, or negative on failure.
#[no_mangle]
pub extern "C" fn iris_ui_camera_open(
    host_ptr: i64,
    device_index: i64,
    req_w: i64,
    req_h: i64,
    synthetic_only: i64,
) -> i64 {
    if host_ptr == 0 {
        set_global_error("invalid host handle in iris_ui_camera_open");
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    let w = if req_w > 0 { req_w as u32 } else { 640 };
    let h = if req_h > 0 { req_h as u32 } else { 360 };
    match host.camera_open(device_index, w, h, synthetic_only != 0) {
        Ok(id) => id,
        Err(err) => {
            host.set_last_error(err.clone());
            set_global_error(err);
            -2
        }
    }
}

/// Stop and release camera stream `cam_id`.
#[no_mangle]
pub extern "C" fn iris_ui_camera_close(host_ptr: i64, cam_id: i64) -> i64 {
    if host_ptr == 0 {
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &mut *(host_ptr as *mut UiHost) };
    if host.camera_close(cam_id) {
        0
    } else {
        -1
    }
}

/// Inspect camera metadata into `out_cell[..out_cap]`:
/// `[width: i32, height: i32, mode: i32, seq_lo: i32]` followed at byte 16 by NUL-terminated UTF-8 name.
#[no_mangle]
pub extern "C" fn iris_ui_camera_info(
    host_ptr: i64,
    cam_id: i64,
    out_cell: i64,
    out_cap: i64,
) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap < 16 {
        return -1;
    }
    // SAFETY: `host_ptr` is a live `UiHost` handle and `out_cell` covers `out_cap >= 16` writable bytes.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    let Some(info) = host.camera_info(cam_id) else {
        return -1;
    };
    let dst = out_cell as *mut u8;
    // SAFETY: `out_cap >= 16` guarantees 4 `i32` slots at offsets 0..16.
    unsafe {
        write_out_i32(dst, 0, info.width as i32);
        write_out_i32(dst, 1, info.height as i32);
        write_out_i32(dst, 2, info.mode);
        write_out_i32(dst, 3, (info.seq & 0x7FFF_FFFF) as i32);
    }
    if out_cap > 16 {
        let name_len = write_utf8_out(&info.device_name, out_cell + 16, out_cap - 16);
        16 + name_len
    } else {
        16
    }
}

/// Downsample camera `cam_id` into `out_cell` (`grid_w * grid_h * 4` straight RGBA8 bytes).
/// `packed_dims_mirror = (grid_w & 0xFFFF) | ((grid_h & 0xFFFF) << 16) | ((mirror & 1) << 32)`.
/// Returns current frame sequence number (`>= 0`), or negative on error.
#[no_mangle]
pub extern "C" fn iris_ui_camera_sample_grid(
    host_ptr: i64,
    cam_id: i64,
    packed_dims_mirror: i64,
    out_cell: i64,
    out_cap: i64,
) -> i64 {
    if host_ptr == 0 || out_cell == 0 || out_cap <= 0 {
        return -1;
    }
    let gw = (packed_dims_mirror & 0xFFFF).clamp(4, 512) as u32;
    let gh = ((packed_dims_mirror >> 16) & 0xFFFF).clamp(4, 512) as u32;
    let mirror = ((packed_dims_mirror >> 32) & 1) != 0;
    let need = (gw as usize) * (gh as usize) * 4;
    if (out_cap as usize) < need {
        return -2;
    }
    // SAFETY: `host_ptr` is live and `out_cell[..need]` is writable within `out_cap`.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    let out_slice = unsafe { std::slice::from_raw_parts_mut(out_cell as *mut u8, need) };
    match host.camera_sample_grid(cam_id, gw, gh, mirror, out_slice) {
        Some(seq) => (seq & 0x7FFF_FFFF) as i64,
        None => -1,
    }
}

/// Configure a target inside synthetic camera feed `cam_id`.
/// `slot_alive_flash = (slot & 0xFF) | ((alive & 1) << 8) | ((hit_flash & 0xFF) << 16)`.
/// `nx_e4`, `ny_e4`, `radius_e4` encode normalized coordinates scaled by `10_000`.
#[no_mangle]
pub extern "C" fn iris_ui_camera_set_sim_target(
    host_ptr: i64,
    cam_id: i64,
    slot_alive_flash: i64,
    nx_e4: i64,
    ny_e4: i64,
    radius_e4: i64,
) -> i64 {
    if host_ptr == 0 {
        return -1;
    }
    let slot = (slot_alive_flash & 0xFF) as usize;
    let alive = ((slot_alive_flash >> 8) & 1) != 0;
    let hit_flash = ((slot_alive_flash >> 16) & 0xFF) as u8;
    let spec = SimTargetSpec {
        nx: (nx_e4 as f32) / 10_000.0,
        ny: (ny_e4 as f32) / 10_000.0,
        radius: (radius_e4 as f32) / 10_000.0,
        alive,
        hit_flash,
    };
    // SAFETY: `host_ptr` is a live `UiHost` handle created by `iris_ui_host_create`.
    let host = unsafe { &*(host_ptr as *const UiHost) };
    if host.camera_set_sim_target(cam_id, slot, spec) {
        0
    } else {
        -1
    }
}
