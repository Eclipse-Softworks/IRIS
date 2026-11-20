use std::ffi::CString;

use iris_ui_host::abi::{
    iris_ui_copy_rgba, iris_ui_framebuffer_dims, iris_ui_get_metrics, iris_ui_host_create,
    iris_ui_host_destroy, iris_ui_inject_event, iris_ui_is_open, iris_ui_last_error,
    iris_ui_measure_text, iris_ui_poll_events, iris_ui_sample_pixel, iris_ui_save_png,
    iris_ui_submit_frame, iris_ui_version,
};
use iris_ui_host::protocol::{
    decode_event_record, encode_event_record, EventKind, HostMode, RgbaColor, UiHostEvent,
    DRAW_PACKET_MAGIC, DRAW_PACKET_VERSION, EVENT_RECORD_BYTES,
};
use iris_ui_host::window::UiHost;

fn push_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_bits().to_le_bytes());
}

fn push_padded_str(buf: &mut Vec<u8>, s: &str) {
    let raw = s.as_bytes();
    push_i32(buf, raw.len() as i32);
    buf.extend_from_slice(raw);
    let pad = (4 - (raw.len() % 4)) % 4;
    buf.extend(std::iter::repeat_n(0u8, pad));
}

fn push_radii(buf: &mut Vec<u8>, radii: [f32; 4]) {
    for r in radii {
        push_f32(buf, r);
    }
}

fn push_transform(buf: &mut Vec<u8>, t: [f32; 6]) {
    for v in t {
        push_f32(buf, v);
    }
}

fn near(actual: u8, expected: u8, tol: u8) -> bool {
    actual.abs_diff(expected) <= tol
}

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    RgbaColor { r, g, b, a }.to_packed_u32()
}

#[test]
fn multiwindow_headless_routes_frames_and_events() {
    let mut host = UiHost::create(HostMode::Headless, "Primary", 320, 240, 1000)
        .expect("headless host must open");
    let id2 = host
        .create_window("Secondary", 300, 200)
        .expect("second window must open");
    assert_eq!(id2, 2);
    assert!(host.window_is_open(1));
    assert!(host.window_is_open(2));

    let mut ev_cell = [0u8; EVENT_RECORD_BYTES];
    encode_event_record(
        &UiHostEvent {
            window_id: 2,
            kind: EventKind::PointerDown,
            target_or_button: 0,
            modifiers: 0,
            key_code: 0,
            x: 11.0,
            y: 12.0,
            text: String::new(),
        },
        &mut ev_cell,
    );
    host.inject_event(decode_event_record(&ev_cell).expect("event decodes"));

    let mut out = [0u8; EVENT_RECORD_BYTES * 2];
    assert_eq!(
        host.poll_events_for_window(2, 0, &mut out).expect("poll 2"),
        1
    );
    assert_eq!(
        host.poll_events_for_window(1, 0, &mut out).expect("poll 1"),
        0
    );
    let routed = decode_event_record(&out[0..EVENT_RECORD_BYTES]).expect("routed event decodes");
    assert_eq!(routed.window_id, 2);
    assert_eq!(routed.x, 11.0);

    host.close_window(2).expect("close secondary");
    assert!(!host.window_is_open(2));
    assert!(host.is_open(), "primary window keeps the host alive");
}

#[test]
fn headless_render_clip_text_tensor_a11y_and_png() {
    let mut host = UiHost::create(HostMode::Headless, "IRIS Test Studio", 400, 300, 1500)
        .expect("headless host creation must succeed");
    assert_eq!(host.mode(), HostMode::Headless);
    assert!(host.is_open());

    let metrics = host.metrics();
    assert_eq!(metrics.logical_width, 400);
    assert_eq!(metrics.logical_height, 300);
    assert_eq!(metrics.scale_milli, 1500);

    // Measure text via cosmic-text.
    let mut mreq = Vec::new();
    push_f32(&mut mreq, 16.0);
    push_f32(&mut mreq, 22.0);
    push_f32(&mut mreq, 280.0);
    push_i32(&mut mreq, 1); // bold
    let text_sample = "IRIS Obsidian & Signal";
    push_i32(&mut mreq, text_sample.len() as i32);
    mreq.extend_from_slice(text_sample.as_bytes());
    let tm = host
        .measure_text(&mreq)
        .expect("measure_text should succeed");
    assert!(
        tm.width > 10.0,
        "expected positive text width, got {}",
        tm.width
    );
    assert!(
        tm.height >= 22.0,
        "expected height >= 22, got {}",
        tm.height
    );
    assert!(tm.lines >= 1);

    // Build DrawPacket exercising every command kind.
    let mut pkt = Vec::new();
    push_u32(&mut pkt, DRAW_PACKET_MAGIC);
    push_u32(&mut pkt, DRAW_PACKET_VERSION);
    push_u32(&mut pkt, rgba(0x1E, 0x1E, 0x1E, 0xFF)); // IRIS Obsidian bg
    push_i32(&mut pkt, 9); // 9 commands

    // 1. Orchid panel card
    push_i32(&mut pkt, 1);
    push_f32(&mut pkt, 20.0);
    push_f32(&mut pkt, 20.0);
    push_f32(&mut pkt, 160.0);
    push_f32(&mut pkt, 80.0);
    push_f32(&mut pkt, 10.0);
    push_u32(&mut pkt, rgba(0xC5, 0x86, 0xC0, 0xFF));
    push_u32(&mut pkt, rgba(0x4E, 0xC9, 0xB0, 0xFF));
    push_f32(&mut pkt, 2.0);

    // 2. Dashed radar reference line
    push_i32(&mut pkt, 2);
    push_f32(&mut pkt, 20.0);
    push_f32(&mut pkt, 120.0);
    push_f32(&mut pkt, 380.0);
    push_f32(&mut pkt, 120.0);
    push_u32(&mut pkt, rgba(0x4F, 0xC1, 0xFF, 0xFF));
    push_f32(&mut pkt, 2.0);
    push_i32(&mut pkt, 1);

    // 3. Telemetry blip circle
    push_i32(&mut pkt, 3);
    push_f32(&mut pkt, 250.0);
    push_f32(&mut pkt, 80.0);
    push_f32(&mut pkt, 24.0);
    push_u32(&mut pkt, rgba(0x4E, 0xC9, 0xB0, 0xFF));
    push_u32(&mut pkt, rgba(0xDC, 0xDC, 0xAA, 0xFF));
    push_f32(&mut pkt, 2.0);

    // 4. Polyline waveform
    push_i32(&mut pkt, 4);
    push_i32(&mut pkt, 3);
    push_u32(&mut pkt, rgba(0x56, 0x9C, 0xD6, 0xFF));
    push_f32(&mut pkt, 2.0);
    push_u32(&mut pkt, rgba(0x56, 0x9C, 0xD6, 0x40));
    for (px, py) in [(20.0, 260.0), (140.0, 210.0), (360.0, 250.0)] {
        push_f32(&mut pkt, px);
        push_f32(&mut pkt, py);
    }

    // 5-7. Clip test: PushClip to (300, 180, 40, 40), draw oversized rect, PopClip.
    push_i32(&mut pkt, 5);
    push_f32(&mut pkt, 300.0);
    push_f32(&mut pkt, 180.0);
    push_f32(&mut pkt, 40.0);
    push_f32(&mut pkt, 40.0);

    push_i32(&mut pkt, 1);
    push_f32(&mut pkt, 280.0);
    push_f32(&mut pkt, 170.0);
    push_f32(&mut pkt, 100.0);
    push_f32(&mut pkt, 80.0);
    push_f32(&mut pkt, 0.0);
    push_u32(&mut pkt, rgba(0xCE, 0x91, 0x78, 0xFF)); // coral fill
    push_u32(&mut pkt, 0);
    push_f32(&mut pkt, 0.0);

    push_i32(&mut pkt, 6);

    // 8. Text + Tensor image (2x2 distinct quadrants)
    push_i32(&mut pkt, 7);
    push_f32(&mut pkt, 32.0);
    push_f32(&mut pkt, 130.0);
    push_f32(&mut pkt, 300.0);
    push_f32(&mut pkt, 16.0);
    push_f32(&mut pkt, 22.0);
    push_u32(&mut pkt, rgba(0xD4, 0xD4, 0xD4, 0xFF));
    push_i32(&mut pkt, 2); // monospace
    push_padded_str(&mut pkt, "RADAR OK");

    // 9. A11y button node
    push_i32(&mut pkt, 9);
    push_i32(&mut pkt, 42);
    push_i32(&mut pkt, 0);
    push_i32(&mut pkt, 3); // Button
    push_f32(&mut pkt, 20.0);
    push_f32(&mut pkt, 20.0);
    push_f32(&mut pkt, 160.0);
    push_f32(&mut pkt, 80.0);
    push_i32(&mut pkt, 4); // focused
    push_padded_str(&mut pkt, "Engage Helm");

    host.submit_frame(&pkt).expect("submit_frame must succeed");
    assert_eq!(host.framebuffer_dims(), (600, 450)); // 400x300 * 1.5

    // Background pixel outside geometry stays Obsidian #1E1E1E.
    let bg = host.sample_pixel(5, 5).expect("in bounds");
    assert_eq!((bg.r, bg.g, bg.b, bg.a), (0x1E, 0x1E, 0x1E, 0xFF));

    // Inside card at logical (60, 60) -> physical (90, 90) is Orchid #C586C0.
    let card = host.sample_pixel(90, 90).expect("in bounds");
    assert_eq!((card.r, card.g, card.b), (0xC5, 0x86, 0xC0));

    // Inside clip (320, 200) -> physical (480, 300) is Coral #CE9178, while (285, 175) -> (427, 262) stays background.
    let clipped_in = host.sample_pixel(480, 300).expect("in bounds");
    assert_eq!(
        (clipped_in.r, clipped_in.g, clipped_in.b),
        (0xCE, 0x91, 0x78)
    );
    let clipped_out = host.sample_pixel(427, 262).expect("in bounds");
    assert_eq!(
        (clipped_out.r, clipped_out.g, clipped_out.b),
        (0x1E, 0x1E, 0x1E)
    );

    // Verify accessibility tree captured the focused button.
    assert_eq!(host.a11y_node_count(), 1);
    let tree_update = host.a11y_node_count();
    assert_eq!(tree_update, 1);

    // Save PNG and check signature.
    let png_path = std::env::temp_dir().join("iris_ui_host_smoke_test.png");
    host.save_png(&png_path).expect("PNG export should succeed");
    let png_bytes = std::fs::read(&png_path).expect("saved PNG must be readable");
    assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let _ = std::fs::remove_file(&png_path);
}

#[test]
fn headless_extended_draw_opcodes_render() {
    let mut host = UiHost::create(HostMode::Headless, "Extended Opcodes", 320, 240, 1000)
        .expect("headless host must open");

    let mut pkt = Vec::new();
    push_u32(&mut pkt, DRAW_PACKET_MAGIC);
    push_u32(&mut pkt, DRAW_PACKET_VERSION);
    push_u32(&mut pkt, rgba(0x14, 0x14, 0x14, 0xFF));
    push_i32(&mut pkt, 10);

    // 10: per-corner radii (only the top-left corner is rounded).
    push_i32(&mut pkt, 10);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 100.0);
    push_f32(&mut pkt, 60.0);
    push_radii(&mut pkt, [20.0, 0.0, 0.0, 0.0]);
    push_u32(&mut pkt, rgba(0xFF, 0x00, 0xFF, 0xFF));
    push_u32(&mut pkt, rgba(0, 0, 0, 0));
    push_f32(&mut pkt, 0.0);

    // 11: linear gradient red -> blue across x.
    push_i32(&mut pkt, 11);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 80.0);
    push_f32(&mut pkt, 100.0);
    push_f32(&mut pkt, 40.0);
    push_radii(&mut pkt, [0.0; 4]);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 80.0);
    push_f32(&mut pkt, 110.0);
    push_f32(&mut pkt, 80.0);
    push_i32(&mut pkt, 0);
    push_i32(&mut pkt, 2);
    push_f32(&mut pkt, 0.0);
    push_u32(&mut pkt, rgba(0xFF, 0, 0, 0xFF));
    push_f32(&mut pkt, 1.0);
    push_u32(&mut pkt, rgba(0, 0, 0xFF, 0xFF));

    // 12: radial gradient white center -> black rim.
    push_i32(&mut pkt, 12);
    push_f32(&mut pkt, 120.0);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 80.0);
    push_f32(&mut pkt, 80.0);
    push_radii(&mut pkt, [0.0; 4]);
    push_f32(&mut pkt, 160.0);
    push_f32(&mut pkt, 50.0);
    push_f32(&mut pkt, 40.0);
    push_i32(&mut pkt, 0);
    push_i32(&mut pkt, 2);
    push_f32(&mut pkt, 0.0);
    push_u32(&mut pkt, rgba(0xFF, 0xFF, 0xFF, 0xFF));
    push_f32(&mut pkt, 1.0);
    push_u32(&mut pkt, rgba(0, 0, 0, 0xFF));

    // 13: blurred shadow, then the opaque card it belongs to.
    push_i32(&mut pkt, 13);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 140.0);
    push_f32(&mut pkt, 100.0);
    push_f32(&mut pkt, 40.0);
    push_radii(&mut pkt, [4.0, 4.0, 4.0, 4.0]);
    push_f32(&mut pkt, 4.0);
    push_f32(&mut pkt, 4.0);
    push_f32(&mut pkt, 4.0);
    push_f32(&mut pkt, 0.0);
    push_u32(&mut pkt, rgba(0, 0, 0, 0xC8));
    push_i32(&mut pkt, 1); // FillRoundRect
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 140.0);
    push_f32(&mut pkt, 100.0);
    push_f32(&mut pkt, 40.0);
    push_f32(&mut pkt, 4.0);
    push_u32(&mut pkt, rgba(0xFF, 0xFF, 0xFF, 0xFF));
    push_u32(&mut pkt, rgba(0, 0, 0, 0));
    push_f32(&mut pkt, 0.0);

    // 14: arbitrary path (triangle) + 15/16: transform stack.
    for opcode in [14, 15, 14, 16] {
        if opcode == 15 {
            push_i32(&mut pkt, 15);
            push_transform(&mut pkt, [1.0, 0.0, 0.0, 1.0, 50.0, 0.0]);
            continue;
        }
        if opcode == 16 {
            push_i32(&mut pkt, 16);
            continue;
        }
        push_i32(&mut pkt, 14);
        push_transform(&mut pkt, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        push_i32(&mut pkt, 4);
        push_i32(&mut pkt, 1);
        push_f32(&mut pkt, 200.0);
        push_f32(&mut pkt, 10.0);
        push_i32(&mut pkt, 2);
        push_f32(&mut pkt, 240.0);
        push_f32(&mut pkt, 10.0);
        push_i32(&mut pkt, 2);
        push_f32(&mut pkt, 220.0);
        push_f32(&mut pkt, 50.0);
        push_i32(&mut pkt, 5);
        push_u32(&mut pkt, rgba(0x00, 0xFF, 0x00, 0xFF));
        push_u32(&mut pkt, rgba(0, 0, 0, 0));
        push_f32(&mut pkt, 0.0);
        push_i32(&mut pkt, 0);
    }

    // 17: SVG path data rasterised into a viewport.
    push_i32(&mut pkt, 17);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 200.0);
    push_f32(&mut pkt, 100.0);
    push_f32(&mut pkt, 30.0);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 10.0);
    push_transform(&mut pkt, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    push_padded_str(&mut pkt, "M0 0 L10 0 L10 10 Z");
    push_u32(&mut pkt, rgba(0x00, 0xFF, 0xFF, 0xFF));
    push_u32(&mut pkt, rgba(0, 0, 0, 0));
    push_f32(&mut pkt, 0.0);

    host.submit_frame(&pkt)
        .expect("extended packet must decode and render");

    let px = |x, y| host.sample_pixel(x, y).expect("in bounds");
    // Per-corner radius: centre is magenta, the clipped top-left corner is background.
    let corner_fill = px(60, 40);
    assert!(near(corner_fill.r, 0xFF, 8) && near(corner_fill.b, 0xFF, 8));
    let corner_cut = px(11, 11);
    assert!(near(corner_cut.r, 0x14, 8) && near(corner_cut.g, 0x14, 8));
    // Linear gradient: left red-dominant, right blue-dominant.
    let grad_left = px(30, 100);
    let grad_right = px(90, 100);
    assert!(
        grad_left.r > grad_left.b + 40,
        "left stop should be red: {grad_left:?}"
    );
    assert!(
        grad_right.b > grad_right.r + 40,
        "right stop should be blue: {grad_right:?}"
    );
    // Radial gradient: bright centre, dark rim.
    let radial_center = px(160, 50);
    let radial_rim = px(122, 14);
    assert!(
        radial_center.r > radial_rim.r + 40,
        "radial centre should be brighter"
    );
    // Shadow darkens the area just outside the card.
    let shadow = px(112, 182);
    let clear = px(112, 130);
    assert!(
        shadow.r < clear.r.saturating_sub(8),
        "shadow should darken neighbours"
    );
    // Path + transform stack: the same triangle appears at both offsets.
    let path1 = px(220, 20);
    let path2 = px(270, 20);
    assert!(
        near(path1.g, 0xFF, 12),
        "first triangle should be green: {path1:?}"
    );
    assert!(
        near(path2.g, 0xFF, 12),
        "transformed triangle should be green: {path2:?}"
    );
    // SVG path data.
    let svg = px(50, 210);
    assert!(
        near(svg.g, 0xFF, 12) && near(svg.b, 0xFF, 12),
        "SVG fill should be cyan: {svg:?}"
    );
}

#[test]
fn c_abi_roundtrip_tensor_image_and_events() {
    assert_eq!(iris_ui_version(), 0x0001_0000);

    let title = CString::new("ABI Host").expect("valid cstr");
    // SAFETY: `title` is a valid CString alive for the duration of the call.
    let host = unsafe { iris_ui_host_create(1, title.as_ptr(), 120, 80, 1000) };
    assert_ne!(host, 0);
    assert_eq!(iris_ui_is_open(host), 1);

    let mut metrics = [0u8; 16];
    assert_eq!(
        iris_ui_get_metrics(host, metrics.as_mut_ptr() as i64, 16),
        0
    );
    assert_eq!(i32::from_le_bytes(metrics[0..4].try_into().unwrap()), 120);
    assert_eq!(i32::from_le_bytes(metrics[4..8].try_into().unwrap()), 80);

    // Measure text via C ABI.
    let mut req = Vec::new();
    push_f32(&mut req, 14.0);
    push_f32(&mut req, 0.0);
    push_f32(&mut req, 0.0);
    push_i32(&mut req, 0);
    push_i32(&mut req, 3);
    req.extend_from_slice(b"AIS");
    let mut m_out = [0u8; 12];
    assert_eq!(
        iris_ui_measure_text(
            host,
            req.as_ptr() as i64,
            req.len() as i64,
            m_out.as_mut_ptr() as i64,
            12
        ),
        0
    );
    let mw = f32::from_le_bytes(m_out[0..4].try_into().unwrap());
    assert!(mw > 0.0);

    // Submit frame with a 2x2 RGBA tensor texture.
    let mut pkt = Vec::new();
    push_u32(&mut pkt, DRAW_PACKET_MAGIC);
    push_u32(&mut pkt, DRAW_PACKET_VERSION);
    push_u32(&mut pkt, rgba(0x10, 0x10, 0x14, 0xFF));
    push_i32(&mut pkt, 1);

    push_i32(&mut pkt, 8); // CMD_IMAGE_RGBA
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 10.0);
    push_f32(&mut pkt, 40.0);
    push_f32(&mut pkt, 40.0);
    push_i32(&mut pkt, 2);
    push_i32(&mut pkt, 2);
    push_u32(&mut pkt, rgba(0xFF, 0x00, 0x00, 0xFF)); // top-left red
    push_u32(&mut pkt, rgba(0x00, 0xFF, 0x00, 0xFF)); // top-right green
    push_u32(&mut pkt, rgba(0x00, 0x00, 0xFF, 0xFF)); // bottom-left blue
    push_u32(&mut pkt, rgba(0xFF, 0xFF, 0x00, 0xFF)); // bottom-right yellow

    assert_eq!(
        iris_ui_submit_frame(host, pkt.as_ptr() as i64, pkt.len() as i64),
        0
    );
    let mut dims = [0u8; 8];
    assert_eq!(
        iris_ui_framebuffer_dims(host, dims.as_mut_ptr() as i64, 8),
        0
    );
    assert_eq!(i32::from_le_bytes(dims[0..4].try_into().unwrap()), 120);
    assert_eq!(i32::from_le_bytes(dims[4..8].try_into().unwrap()), 80);

    let px = iris_ui_sample_pixel(host, 14, 14) as u32;
    let c = RgbaColor::from_packed_u32(px);
    assert!(
        c.r > 200 && c.g < 40,
        "expected red top-left texel, got {c:?}"
    );

    let mut rgba_copy = vec![0u8; 120 * 80 * 4];
    assert_eq!(
        iris_ui_copy_rgba(host, rgba_copy.as_mut_ptr() as i64, rgba_copy.len() as i64),
        (120 * 80 * 4) as i64
    );

    // Inject PointerDown and TextInput events and poll them back.
    let mut ev1 = [0u8; EVENT_RECORD_BYTES];
    encode_event_record(
        &UiHostEvent {
            window_id: 1,
            kind: EventKind::PointerDown,
            target_or_button: 0,
            modifiers: 2,
            key_code: 0,
            x: 33.5,
            y: 44.25,
            text: String::new(),
        },
        &mut ev1,
    );
    assert_eq!(iris_ui_inject_event(host, ev1.as_ptr() as i64, 64), 0);

    let mut ev2 = [0u8; EVENT_RECORD_BYTES];
    encode_event_record(
        &UiHostEvent {
            window_id: 1,
            kind: EventKind::TextInput,
            target_or_button: 0,
            modifiers: 0,
            key_code: 0,
            x: 0.0,
            y: 0.0,
            text: "Helsinki-TSS".to_owned(),
        },
        &mut ev2,
    );
    assert_eq!(iris_ui_inject_event(host, ev2.as_ptr() as i64, 64), 0);

    let mut out_ev = vec![0u8; EVENT_RECORD_BYTES * 4];
    let n = iris_ui_poll_events(host, 0, out_ev.as_mut_ptr() as i64, out_ev.len() as i64);
    assert_eq!(n, 2);

    let dec1 = decode_event_record(&out_ev[0..64]).expect("event 1 decodes");
    assert_eq!(dec1.kind, EventKind::PointerDown);
    assert_eq!(dec1.modifiers, 2);
    assert_eq!(dec1.x, 33.5);
    assert_eq!(dec1.y, 44.25);

    let dec2 = decode_event_record(&out_ev[64..128]).expect("event 2 decodes");
    assert_eq!(dec2.kind, EventKind::TextInput);
    assert_eq!(dec2.text, "Helsinki-TSS");

    // Verify invalid command magic records clear error string.
    let bad = [0u8; 16];
    assert!(iris_ui_submit_frame(host, bad.as_ptr() as i64, 16) < 0);
    let mut err_buf = [0u8; 128];
    let elen = iris_ui_last_error(host, err_buf.as_mut_ptr() as i64, 128);
    assert!(elen > 0);
    let err_str = std::str::from_utf8(&err_buf[..elen as usize]).unwrap();
    assert!(err_str.contains("magic"));

    // PNG C ABI error on empty path check (positive PNG path tested above).
    let png_out = std::env::temp_dir().join("iris_ui_abi_test.png");
    let c_png = CString::new(png_out.to_string_lossy().as_ref()).unwrap();
    // SAFETY: `c_png` is a valid CString alive for the duration of the call.
    assert_eq!(unsafe { iris_ui_save_png(host, c_png.as_ptr()) }, 0);
    let _ = std::fs::remove_file(&png_out);

    assert_eq!(iris_ui_host_destroy(host), 0);
}
