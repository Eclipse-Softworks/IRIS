use std::cell::RefCell;
use std::convert::TryFrom;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;

use crate::protocol::{decode_event_record, HostMode};
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
