use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use accesskit_winit::Adapter as A11yAdapter;
use softbuffer::{Context as SoftContext, Surface as SoftSurface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::{CursorIcon, Fullscreen, Icon, Window, WindowAttributes, WindowId};

use crate::accessibility::A11yTreeState;
use crate::camera::{CameraInfo, CameraManager, SimTargetSpec};
use crate::clipboard;
use crate::protocol::{
    decode_draw_packet, decode_measure_request, encode_event_record, CursorKind, EventKind,
    HostMode, RgbaColor, UiHostEvent, EVENT_RECORD_BYTES,
};
use crate::renderer::{RendererBackend, TinySkiaBackend};
use crate::text::{TextEngine, TextMetrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostMetrics {
    pub logical_width: u32,
    pub logical_height: u32,
    pub scale_milli: u32,
    pub is_open: bool,
}

/// All state that belongs to one native window. Window 1 is the primary
/// window created with the host; additional windows are created with
/// `UiHost::create_window`.
struct WindowState {
    id: i64,
    title: String,
    logical_width: u32,
    logical_height: u32,
    scale: f32,
    is_open: bool,
    cursor: CursorKind,
    resizable: bool,
    fullscreen: bool,
    maximized: bool,
    events: VecDeque<UiHostEvent>,
    a11y: A11yTreeState,
    last_a11y_fingerprint: u64,
    renderer: Box<dyn RendererBackend>,
    // Native presentation. Both are None for headless hosts.
    surface: Option<SoftSurface<Arc<Window>, Arc<Window>>>,
    last_surface_size: (u32, u32),
    _context: Option<SoftContext<Arc<Window>>>,
}

impl WindowState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: i64,
        title: String,
        logical_width: u32,
        logical_height: u32,
        scale: f32,
        renderer: Box<dyn RendererBackend>,
        surface: Option<SoftSurface<Arc<Window>, Arc<Window>>>,
        context: Option<SoftContext<Arc<Window>>>,
    ) -> Self {
        Self {
            id,
            title,
            logical_width,
            logical_height,
            scale,
            is_open: true,
            cursor: CursorKind::Default,
            resizable: true,
            fullscreen: false,
            maximized: false,
            events: VecDeque::new(),
            a11y: A11yTreeState::default(),
            last_a11y_fingerprint: 0,
            renderer,
            surface,
            last_surface_size: (0, 0),
            _context: context,
        }
    }
}

pub struct UiHost {
    mode: HostMode,
    /// Index 0 is always the primary window (public id 1).
    windows: Vec<WindowState>,
    text: TextEngine,
    cameras: CameraManager,
    native: Option<NativeState>,
    started_at: Instant,
    headless_clipboard: String,
    dropped_files: VecDeque<String>,
    last_error: String,
}

struct NativeState {
    event_loop: EventLoop<accesskit_winit::Event>,
    app: NativeAppHandler,
    thread_id: std::thread::ThreadId,
}

struct NativeWinState {
    window: Arc<Window>,
    a11y_adapter: Option<A11yAdapter>,
    /// Public 1-based window id carried with every event.
    id: i64,
    title: String,
    last_scale: f32,
    last_cursor_pos: (f32, f32),
    last_cursor: CursorKind,
    is_open: bool,
}

struct NativeAppHandler {
    windows: std::collections::HashMap<WindowId, NativeWinState>,
    proxy: winit::event_loop::EventLoopProxy<accesskit_winit::Event>,
    primary_title: String,
    primary_width: u32,
    primary_height: u32,
    modifiers: ModifiersState,
    pending_events: VecDeque<UiHostEvent>,
    dropped_files: VecDeque<String>,
    init_error: Option<String>,
}

impl NativeAppHandler {
    fn new(
        proxy: winit::event_loop::EventLoopProxy<accesskit_winit::Event>,
        title: String,
        logical_width: u32,
        logical_height: u32,
    ) -> Self {
        Self {
            windows: std::collections::HashMap::new(),
            proxy,
            primary_title: title,
            primary_width: logical_width,
            primary_height: logical_height,
            modifiers: ModifiersState::empty(),
            pending_events: VecDeque::new(),
            dropped_files: VecDeque::new(),
            init_error: None,
        }
    }

    fn register_window(&mut self, window: Arc<Window>, id: i64, title: String, scale: f32) {
        let adapter = A11yAdapter::with_event_loop_proxy(&window, self.proxy());
        self.windows.insert(
            window.id(),
            NativeWinState {
                window,
                a11y_adapter: Some(adapter),
                id,
                title,
                last_scale: scale,
                last_cursor_pos: (0.0, 0.0),
                last_cursor: CursorKind::Default,
                is_open: true,
            },
        );
    }

    fn unregister_window(&mut self, window_id: WindowId) {
        self.windows.remove(&window_id);
    }

    fn proxy(&self) -> winit::event_loop::EventLoopProxy<accesskit_winit::Event> {
        self.proxy.clone()
    }

    fn modifier_bits(&self) -> i32 {
        let mut bits = 0;
        if self.modifiers.shift_key() {
            bits |= 1;
        }
        if self.modifiers.control_key() {
            bits |= 2;
        }
        if self.modifiers.alt_key() {
            bits |= 4;
        }
        if self.modifiers.super_key() {
            bits |= 8;
        }
        bits
    }
}

impl ApplicationHandler<accesskit_winit::Event> for NativeAppHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title(self.primary_title.clone())
            .with_inner_size(LogicalSize::new(
                self.primary_width as f64,
                self.primary_height as f64,
            ))
            .with_visible(false);
        match event_loop.create_window(attrs) {
            Ok(win) => {
                let win = Arc::new(win);
                let scale = win.scale_factor() as f32;
                win.set_visible(true);
                self.register_window(win, 1, self.primary_title.clone(), scale);
            }
            Err(err) => {
                self.init_error = Some(format!("failed to create winit window: {err}"));
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: accesskit_winit::Event) {
        if !matches!(
            event.window_event,
            accesskit_winit::WindowEvent::InitialTreeRequested
        ) {
            return;
        }
        for ws in self.windows.values_mut() {
            if let Some(adapter) = ws.a11y_adapter.as_mut() {
                let title = ws.title.clone();
                adapter.update_if_active(|| A11yTreeState::default().build_tree_update(&title));
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let wid: i64;
        let scale: f32;
        let cursor_pos: (f32, f32);
        {
            let Some(ws) = self.windows.get_mut(&window_id) else {
                return;
            };
            if let Some(adapter) = ws.a11y_adapter.as_mut() {
                adapter.process_event(&ws.window, &event);
            }
            wid = ws.id;
            scale = ws.last_scale.max(0.5);
            cursor_pos = ws.last_cursor_pos;
        }
        match event {
            WindowEvent::CloseRequested => {
                if let Some(ws) = self.windows.get_mut(&window_id) {
                    ws.is_open = false;
                }
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::CloseRequested,
                    target_or_button: 0,
                    modifiers: 0,
                    key_code: 0,
                    x: 0.0,
                    y: 0.0,
                    text: String::new(),
                });
            }
            WindowEvent::Resized(size) => {
                let lw = ((size.width as f32) / scale).round().max(1.0);
                let lh = ((size.height as f32) / scale).round().max(1.0);
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::Resized,
                    target_or_button: 0,
                    modifiers: 0,
                    key_code: 0,
                    x: lw,
                    y: lh,
                    text: String::new(),
                });
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(ws) = self.windows.get_mut(&window_id) {
                    ws.last_scale = scale_factor as f32;
                }
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::ScaleChanged,
                    target_or_button: 0,
                    modifiers: 0,
                    key_code: 0,
                    x: scale_factor as f32,
                    y: 0.0,
                    text: String::new(),
                });
            }
            WindowEvent::CursorMoved { position, .. } => {
                let lx = (position.x as f32) / scale;
                let ly = (position.y as f32) / scale;
                if let Some(ws) = self.windows.get_mut(&window_id) {
                    ws.last_cursor_pos = (lx, ly);
                }
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::PointerMove,
                    target_or_button: 0,
                    modifiers: self.modifier_bits(),
                    key_code: 0,
                    x: lx,
                    y: ly,
                    text: String::new(),
                });
            }
            WindowEvent::CursorLeft { .. } => {
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::PointerLeave,
                    target_or_button: 0,
                    modifiers: self.modifier_bits(),
                    key_code: 0,
                    x: cursor_pos.0,
                    y: cursor_pos.1,
                    text: String::new(),
                });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let btn = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                    _ => 3,
                };
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: if state == ElementState::Pressed {
                        EventKind::PointerDown
                    } else {
                        EventKind::PointerUp
                    },
                    target_or_button: btn,
                    modifiers: self.modifier_bits(),
                    key_code: 0,
                    x: cursor_pos.0,
                    y: cursor_pos.1,
                    text: String::new(),
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 28.0, y * 28.0),
                    MouseScrollDelta::PixelDelta(pos) => {
                        ((pos.x as f32) / scale, (pos.y as f32) / scale)
                    }
                };
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::Wheel,
                    target_or_button: 0,
                    modifiers: self.modifier_bits(),
                    key_code: 0,
                    x: dx,
                    y: dy,
                    text: String::new(),
                });
            }
            WindowEvent::DroppedFile(path) => {
                self.dropped_files
                    .push_back(path.to_string_lossy().into_owned());
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::FileDropped,
                    target_or_button: self.dropped_files.len() as i32,
                    modifiers: self.modifier_bits(),
                    key_code: 0,
                    x: cursor_pos.0,
                    y: cursor_pos.1,
                    text: String::new(),
                });
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                self.modifiers = new_mods.state();
            }
            WindowEvent::KeyboardInput { event: key_ev, .. } => {
                let pressed = key_ev.state == ElementState::Pressed;
                let code = map_logical_key(&key_ev.logical_key);
                let mods = self.modifier_bits();
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: if pressed {
                        EventKind::KeyDown
                    } else {
                        EventKind::KeyUp
                    },
                    target_or_button: 0,
                    modifiers: mods,
                    key_code: code,
                    x: 0.0,
                    y: 0.0,
                    text: String::new(),
                });
                if pressed && !self.modifiers.control_key() && !self.modifiers.super_key() {
                    if let Some(txt) = key_ev.text.as_deref() {
                        push_text_events(&mut self.pending_events, wid, txt);
                    }
                }
            }
            WindowEvent::Ime(Ime::Commit(txt)) => {
                push_text_events(&mut self.pending_events, wid, &txt);
            }
            WindowEvent::Focused(focused) => {
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::FocusChanged,
                    target_or_button: i32::from(focused),
                    modifiers: 0,
                    key_code: 0,
                    x: 0.0,
                    y: 0.0,
                    text: String::new(),
                });
            }
            WindowEvent::RedrawRequested => {
                self.pending_events.push_back(UiHostEvent {
                    window_id: wid,
                    kind: EventKind::RedrawRequested,
                    target_or_button: 0,
                    modifiers: 0,
                    key_code: 0,
                    x: 0.0,
                    y: 0.0,
                    text: String::new(),
                });
            }
            _ => {}
        }
    }
}

impl UiHost {
    pub fn create(
        mode: HostMode,
        title: &str,
        width: u32,
        height: u32,
        scale_milli: u32,
    ) -> Result<Self, String> {
        let lw = width.clamp(64, 8192);
        let lh = height.clamp(64, 8192);
        let initial_scale = if scale_milli > 0 {
            (scale_milli as f32 / 1000.0).clamp(0.5, 4.0)
        } else {
            1.0
        };

        match mode {
            HostMode::Headless => {
                let pw = ((lw as f32) * initial_scale).round().max(1.0) as u32;
                let ph = ((lh as f32) * initial_scale).round().max(1.0) as u32;
                let win = WindowState::new(
                    1,
                    title.to_owned(),
                    lw,
                    lh,
                    initial_scale,
                    Box::new(TinySkiaBackend::new(pw, ph)?),
                    None,
                    None,
                );
                Ok(Self {
                    mode,
                    windows: vec![win],
                    text: TextEngine::new(),
                    cameras: CameraManager::new(),
                    native: None,
                    started_at: Instant::now(),
                    headless_clipboard: String::new(),
                    dropped_files: VecDeque::new(),
                    last_error: String::new(),
                })
            }
            HostMode::NativeWindow => Self::create_native(title, lw, lh, scale_milli),
        }
    }

    fn create_native(title: &str, lw: u32, lh: u32, scale_milli: u32) -> Result<Self, String> {
        let mut event_loop = EventLoop::<accesskit_winit::Event>::with_user_event()
            .build()
            .map_err(|e| format!("failed to build event loop: {e}"))?;
        let proxy = event_loop.create_proxy();
        let mut app = NativeAppHandler::new(proxy, title.to_owned(), lw, lh);
        let _ = event_loop.pump_app_events(Some(Duration::from_millis(0)), &mut app);
        if let Some(err) = app.init_error.take() {
            return Err(err);
        }
        let (window, scale) = {
            let ws = app
                .windows
                .values()
                .find(|ws| ws.id == 1)
                .ok_or_else(|| "window was not initialized on resumed".to_owned())?;
            let scale = if scale_milli > 0 {
                (scale_milli as f32 / 1000.0).clamp(0.5, 4.0)
            } else {
                (ws.window.scale_factor() as f32).clamp(0.5, 4.0)
            };
            (Arc::clone(&ws.window), scale)
        };
        let context = SoftContext::new(Arc::clone(&window))
            .map_err(|e| format!("failed to create softbuffer context: {e}"))?;
        let surface = SoftSurface::new(&context, Arc::clone(&window))
            .map_err(|e| format!("failed to create softbuffer surface: {e}"))?;
        let pw = ((lw as f32) * scale).round().max(1.0) as u32;
        let ph = ((lh as f32) * scale).round().max(1.0) as u32;
        let win = WindowState::new(
            1,
            title.to_owned(),
            lw,
            lh,
            scale,
            Box::new(TinySkiaBackend::new(pw, ph)?),
            Some(surface),
            Some(context),
        );
        Ok(Self {
            mode: HostMode::NativeWindow,
            windows: vec![win],
            text: TextEngine::new(),
            cameras: CameraManager::new(),
            native: Some(NativeState {
                event_loop,
                app,
                thread_id: std::thread::current().id(),
            }),
            started_at: Instant::now(),
            headless_clipboard: String::new(),
            dropped_files: VecDeque::new(),
            last_error: String::new(),
        })
    }

    fn index_for(&self, window_id: i64) -> Option<usize> {
        let id = if window_id <= 0 { 1 } else { window_id };
        self.windows.iter().position(|w| w.id == id)
    }

    fn find_native_window_mut(
        native: &mut NativeState,
        public_id: i64,
    ) -> Option<&mut NativeWinState> {
        let id = if public_id <= 0 { 1 } else { public_id };
        native.app.windows.values_mut().find(|ws| ws.id == id)
    }

    /// Create an additional window in this host. Returns its 1-based id.
    pub fn create_window(&mut self, title: &str, width: u32, height: u32) -> Result<i64, String> {
        let lw = width.clamp(64, 8192);
        let lh = height.clamp(64, 8192);
        let id = self.windows.iter().map(|w| w.id).max().unwrap_or(0) + 1;
        let scale = self.windows[0].scale;
        let (surface, context, actual_scale) = if let Some(native) = self.native.as_mut() {
            if std::thread::current().id() != native.thread_id {
                return Err("iris_ui_window_create called from a non-UI thread".to_owned());
            }
            let attrs = WindowAttributes::default()
                .with_title(title.to_owned())
                .with_inner_size(LogicalSize::new(lw as f64, lh as f64))
                .with_visible(false);
            #[allow(deprecated)]
            let win = Arc::new(
                native
                    .event_loop
                    .create_window(attrs)
                    .map_err(|e| format!("failed to create winit window: {e}"))?,
            );
            let win_scale = (win.scale_factor() as f32).clamp(0.5, 4.0);
            win.set_visible(true);
            native
                .app
                .register_window(Arc::clone(&win), id, title.to_owned(), win_scale);
            let context = SoftContext::new(Arc::clone(&win))
                .map_err(|e| format!("failed to create softbuffer context: {e}"))?;
            let surface = SoftSurface::new(&context, Arc::clone(&win))
                .map_err(|e| format!("failed to create softbuffer surface: {e}"))?;
            (Some(surface), Some(context), win_scale)
        } else {
            (None, None, scale)
        };
        let pw = ((lw as f32) * actual_scale).round().max(1.0) as u32;
        let ph = ((lh as f32) * actual_scale).round().max(1.0) as u32;
        self.windows.push(WindowState::new(
            id,
            title.to_owned(),
            lw,
            lh,
            actual_scale,
            Box::new(TinySkiaBackend::new(pw, ph)?),
            surface,
            context,
        ));
        Ok(id)
    }

    /// Close a window. The host stays alive while any other window is open.
    pub fn close_window(&mut self, window_id: i64) -> Result<(), String> {
        let Some(idx) = self.index_for(window_id) else {
            return Err(format!("unknown window id {window_id}"));
        };
        let id = self.windows[idx].id;
        if let Some(native) = self.native.as_mut() {
            if std::thread::current().id() != native.thread_id {
                return Err("iris_ui_window_close called from a non-UI thread".to_owned());
            }
            let winit_id = native
                .app
                .windows
                .iter()
                .find(|(_, ws)| ws.id == id)
                .map(|(wid, _)| *wid);
            if let Some(wid) = winit_id {
                native.app.unregister_window(wid);
            }
        }
        self.windows.remove(idx);
        Ok(())
    }

    pub fn mode(&self) -> HostMode {
        self.mode
    }

    pub fn elapsed_micros(&self) -> i64 {
        self.started_at.elapsed().as_micros().min(i64::MAX as u128) as i64
    }

    pub fn is_open(&self) -> bool {
        self.windows.iter().any(|w| w.is_open)
    }

    pub fn window_is_open(&self, window_id: i64) -> bool {
        self.index_for(window_id)
            .map(|i| self.windows[i].is_open)
            .unwrap_or(false)
    }

    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    pub fn set_last_error(&mut self, msg: String) {
        self.last_error = msg;
    }

    pub fn metrics(&self) -> HostMetrics {
        self.metrics_for_window(1)
    }

    pub fn metrics_for_window(&self, window_id: i64) -> HostMetrics {
        let Some(i) = self.index_for(window_id) else {
            return HostMetrics {
                logical_width: 0,
                logical_height: 0,
                scale_milli: 0,
                is_open: false,
            };
        };
        let w = &self.windows[i];
        HostMetrics {
            logical_width: w.logical_width,
            logical_height: w.logical_height,
            scale_milli: (w.scale * 1000.0).round() as u32,
            is_open: w.is_open,
        }
    }

    pub fn cursor_for_window(&self, window_id: i64) -> Option<CursorKind> {
        self.index_for(window_id).map(|i| self.windows[i].cursor)
    }

    pub fn window_title(&self, window_id: i64) -> Option<&str> {
        self.index_for(window_id)
            .map(|i| self.windows[i].title.as_str())
    }

    pub fn set_cursor(&mut self, window_id: i64, cursor: CursorKind) -> Result<(), String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let public_id = self.windows[idx].id;
        self.windows[idx].cursor = cursor;
        if let Some(native) = self.native.as_mut() {
            if let Some(ws) = Self::find_native_window_mut(native, public_id) {
                if ws.last_cursor != cursor {
                    ws.window.set_cursor(map_cursor_icon(cursor));
                    ws.last_cursor = cursor;
                }
            }
        }
        Ok(())
    }

    pub fn set_title(&mut self, window_id: i64, title: &str) -> Result<(), String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let public_id = self.windows[idx].id;
        self.windows[idx].title = title.to_owned();
        if let Some(native) = self.native.as_mut() {
            if let Some(ws) = Self::find_native_window_mut(native, public_id) {
                ws.title = title.to_owned();
                ws.window.set_title(title);
            }
        }
        Ok(())
    }

    pub fn set_size_constraints(
        &mut self,
        window_id: i64,
        min_w: u32,
        min_h: u32,
        max_w: u32,
        max_h: u32,
    ) -> Result<(), String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let public_id = self.windows[idx].id;
        if let Some(native) = self.native.as_mut() {
            if let Some(ws) = Self::find_native_window_mut(native, public_id) {
                let min_sz = (min_w > 0 && min_h > 0)
                    .then_some(LogicalSize::new(min_w as f64, min_h as f64));
                let max_sz = (max_w > 0 && max_h > 0)
                    .then_some(LogicalSize::new(max_w as f64, max_h as f64));
                ws.window.set_min_inner_size(min_sz);
                ws.window.set_max_inner_size(max_sz);
            }
        }
        Ok(())
    }

    pub fn set_resizable(&mut self, window_id: i64, resizable: bool) -> Result<(), String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let public_id = self.windows[idx].id;
        self.windows[idx].resizable = resizable;
        if let Some(native) = self.native.as_mut() {
            if let Some(ws) = Self::find_native_window_mut(native, public_id) {
                ws.window.set_resizable(resizable);
            }
        }
        Ok(())
    }

    pub fn set_fullscreen(&mut self, window_id: i64, fullscreen: bool) -> Result<(), String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let public_id = self.windows[idx].id;
        self.windows[idx].fullscreen = fullscreen;
        if let Some(native) = self.native.as_mut() {
            if let Some(ws) = Self::find_native_window_mut(native, public_id) {
                ws.window
                    .set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
            }
        }
        Ok(())
    }

    pub fn set_maximized(&mut self, window_id: i64, maximized: bool) -> Result<(), String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let public_id = self.windows[idx].id;
        self.windows[idx].maximized = maximized;
        if let Some(native) = self.native.as_mut() {
            if let Some(ws) = Self::find_native_window_mut(native, public_id) {
                ws.window.set_maximized(maximized);
            }
        }
        Ok(())
    }

    pub fn set_icon_rgba(
        &mut self,
        window_id: i64,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let public_id = self.windows[idx].id;
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if width == 0 || height == 0 || rgba.len() < expected {
            return Err("invalid RGBA icon dimensions or byte length".to_owned());
        }
        let icon = Icon::from_rgba(rgba[..expected].to_vec(), width, height)
            .map_err(|e| format!("invalid window icon: {e}"))?;
        if let Some(native) = self.native.as_mut() {
            if let Some(ws) = Self::find_native_window_mut(native, public_id) {
                ws.window.set_window_icon(Some(icon));
            }
        }
        Ok(())
    }

    pub fn clipboard_get(&mut self) -> String {
        if self.mode == HostMode::Headless {
            return self.headless_clipboard.clone();
        }
        clipboard::get_system_text().unwrap_or_else(|_| self.headless_clipboard.clone())
    }

    pub fn clipboard_set(&mut self, text: &str) -> Result<(), String> {
        self.headless_clipboard = text.to_owned();
        if self.mode == HostMode::NativeWindow {
            return clipboard::set_system_text(text);
        }
        Ok(())
    }

    pub fn take_dropped_file(&mut self) -> Option<String> {
        self.dropped_files.pop_front()
    }

    pub fn measure_text(&mut self, bytes: &[u8]) -> Result<TextMetrics, String> {
        let req = decode_measure_request(bytes).map_err(|e| e.to_string())?;
        Ok(self.text.measure(&req))
    }

    pub fn submit_frame(&mut self, packet_bytes: &[u8]) -> Result<(), String> {
        self.submit_frame_for_window(1, packet_bytes)
    }

    pub fn submit_frame_for_window(
        &mut self,
        window_id: i64,
        packet_bytes: &[u8],
    ) -> Result<(), String> {
        let packet = decode_draw_packet(packet_bytes).map_err(|e| e.to_string())?;
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        let scale = self.windows[idx].scale;
        let pw = ((self.windows[idx].logical_width as f32) * scale)
            .round()
            .max(1.0) as u32;
        let ph = ((self.windows[idx].logical_height as f32) * scale)
            .round()
            .max(1.0) as u32;
        {
            let win = &mut self.windows[idx];
            win.renderer.resize(pw, ph)?;
            win.renderer.render_packet(
                &packet,
                scale,
                &mut self.text,
                &mut win.a11y,
                Some(&self.cameras),
            )?;
        }
        if let Some(native) = self.native.as_mut() {
            if std::thread::current().id() != native.thread_id {
                return Err("iris_ui_submit_frame called from a non-UI thread".to_owned());
            }
            let win = &mut self.windows[idx];
            let id = win.id;
            let rgba = win.renderer.rgba_bytes();
            if let (Some(surface), Some(nw), Some(nh)) = (
                win.surface.as_mut(),
                NonZeroU32::new(pw),
                NonZeroU32::new(ph),
            ) {
                if win.last_surface_size != (pw, ph) {
                    surface
                        .resize(nw, nh)
                        .map_err(|e| format!("softbuffer resize failed: {e}"))?;
                    win.last_surface_size = (pw, ph);
                }
                let mut buffer = surface
                    .buffer_mut()
                    .map_err(|e| format!("softbuffer buffer_mut failed: {e}"))?;
                let (rgba_chunks, _) = rgba.as_chunks::<4>();
                for (dst, px) in buffer.iter_mut().zip(rgba_chunks) {
                    *dst = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | (px[2] as u32);
                }
                buffer
                    .present()
                    .map_err(|e| format!("softbuffer present failed: {e}"))?;
            }
            let fingerprint = win.a11y.fingerprint();
            if win.last_a11y_fingerprint != fingerprint && win.a11y.node_count() > 0 {
                let title = win.title.clone();
                for ws in native.app.windows.values_mut() {
                    if ws.id == id {
                        if let Some(adapter) = ws.a11y_adapter.as_mut() {
                            let tree = &win.a11y;
                            adapter.update_if_active(|| tree.build_tree_update(&title));
                        }
                    }
                }
                win.last_a11y_fingerprint = fingerprint;
            }
        }
        Ok(())
    }

    pub fn inject_event(&mut self, ev: UiHostEvent) {
        let id = if ev.window_id > 0 { ev.window_id } else { 1 };
        if ev.kind == EventKind::FileDropped && !ev.text.is_empty() {
            self.dropped_files.push_back(ev.text.clone());
        }
        if let Some(idx) = self.index_for(id) {
            if let Some(win) = self.windows.get_mut(idx) {
                if ev.kind == EventKind::CloseRequested {
                    win.is_open = false;
                } else if ev.kind == EventKind::Resized && ev.x >= 1.0 && ev.y >= 1.0 {
                    win.logical_width = ev.x.round() as u32;
                    win.logical_height = ev.y.round() as u32;
                }
                win.events.push_back(ev);
            }
        }
    }

    pub fn poll_events(&mut self, timeout_ms: i64, out_buf: &mut [u8]) -> Result<usize, String> {
        self.poll_events_for_window(1, timeout_ms, out_buf)
    }

    pub fn poll_events_for_window(
        &mut self,
        window_id: i64,
        timeout_ms: i64,
        out_buf: &mut [u8],
    ) -> Result<usize, String> {
        let idx = self
            .index_for(window_id)
            .ok_or_else(|| format!("unknown window id {window_id}"))?;
        if let Some(native) = self.native.as_mut() {
            if std::thread::current().id() != native.thread_id {
                return Err("iris_ui_poll_events called from a non-UI thread".to_owned());
            }
            let wait = if timeout_ms > 0 && self.windows[idx].events.is_empty() {
                Some(Duration::from_millis(timeout_ms as u64))
            } else {
                Some(Duration::from_millis(0))
            };
            let status = native.event_loop.pump_app_events(wait, &mut native.app);
            if matches!(status, PumpStatus::Exit(_)) {
                for w in self.windows.iter_mut() {
                    w.is_open = false;
                }
            }
            while let Some(path) = native.app.dropped_files.pop_front() {
                self.dropped_files.push_back(path);
            }
            while let Some(ev) = native.app.pending_events.pop_front() {
                let wid = if ev.window_id > 0 { ev.window_id } else { 1 };
                if let Some(i) = self.windows.iter().position(|w| w.id == wid) {
                    match ev.kind {
                        EventKind::CloseRequested => self.windows[i].is_open = false,
                        EventKind::Resized if ev.x >= 1.0 && ev.y >= 1.0 => {
                            self.windows[i].logical_width = ev.x.round() as u32;
                            self.windows[i].logical_height = ev.y.round() as u32;
                        }
                        EventKind::ScaleChanged if ev.x >= 0.5 => {
                            self.windows[i].scale = ev.x.clamp(0.5, 4.0);
                        }
                        _ => {}
                    }
                    self.windows[i].events.push_back(ev);
                }
            }
        } else if timeout_ms > 0 && self.windows[idx].events.is_empty() {
            let capped = (timeout_ms as u64).min(50);
            let deadline = Instant::now() + Duration::from_millis(capped);
            while Instant::now() < deadline && self.windows[idx].events.is_empty() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        let max_records = out_buf.len() / EVENT_RECORD_BYTES;
        let mut count = 0usize;
        let mut slot = [0u8; EVENT_RECORD_BYTES];
        while count < max_records {
            let Some(ev) = self.windows[idx].events.pop_front() else {
                break;
            };
            encode_event_record(&ev, &mut slot);
            let off = count * EVENT_RECORD_BYTES;
            out_buf[off..off + EVENT_RECORD_BYTES].copy_from_slice(&slot);
            count += 1;
        }
        Ok(count)
    }

    pub fn rgba_bytes(&self) -> &[u8] {
        self.windows[0].renderer.rgba_bytes()
    }

    pub fn framebuffer_dims(&self) -> (u32, u32) {
        self.windows[0].renderer.dimensions_px()
    }

    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        self.windows[0].renderer.save_png(path)
    }

    pub fn sample_pixel(&self, x_px: u32, y_px: u32) -> Option<RgbaColor> {
        self.windows[0].renderer.sample_pixel(x_px, y_px)
    }

    pub fn a11y_node_count(&self) -> usize {
        self.windows[0].a11y.node_count()
    }

    pub fn camera_open(
        &mut self,
        device_index: i64,
        req_w: u32,
        req_h: u32,
        synthetic_only: bool,
    ) -> Result<i64, String> {
        self.cameras
            .open(device_index, req_w, req_h, synthetic_only)
    }

    pub fn camera_close(&mut self, cam_id: i64) -> bool {
        self.cameras.close(cam_id)
    }

    pub fn camera_info(&self, cam_id: i64) -> Option<CameraInfo> {
        self.cameras.info(cam_id)
    }

    pub fn camera_sample_grid(
        &self,
        cam_id: i64,
        grid_w: u32,
        grid_h: u32,
        mirror: bool,
        out_buf: &mut [u8],
    ) -> Option<u64> {
        self.cameras
            .sample_grid(cam_id, grid_w, grid_h, mirror, out_buf)
    }

    pub fn camera_set_sim_target(&self, cam_id: i64, slot: usize, spec: SimTargetSpec) -> bool {
        self.cameras.set_sim_target(cam_id, slot, spec)
    }
}

fn map_cursor_icon(cursor: CursorKind) -> CursorIcon {
    match cursor {
        CursorKind::Default => CursorIcon::Default,
        CursorKind::Pointer => CursorIcon::Pointer,
        CursorKind::Text => CursorIcon::Text,
        CursorKind::Move => CursorIcon::Move,
        CursorKind::Grab => CursorIcon::Grab,
        CursorKind::Grabbing => CursorIcon::Grabbing,
        CursorKind::ResizeEW => CursorIcon::EwResize,
        CursorKind::ResizeNS => CursorIcon::NsResize,
        CursorKind::NotAllowed => CursorIcon::NotAllowed,
        CursorKind::Wait => CursorIcon::Wait,
    }
}

fn push_text_events(queue: &mut VecDeque<UiHostEvent>, window_id: i64, txt: &str) {
    let mut filtered = String::with_capacity(txt.len());
    for ch in txt.chars() {
        if !ch.is_control() {
            filtered.push(ch);
        }
    }
    if filtered.is_empty() {
        return;
    }
    let mut start = 0;
    while start < filtered.len() {
        let mut end = (start + 32).min(filtered.len());
        while end > start && !filtered.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            break;
        }
        queue.push_back(UiHostEvent {
            window_id,
            kind: EventKind::TextInput,
            target_or_button: 0,
            modifiers: 0,
            key_code: 0,
            x: 0.0,
            y: 0.0,
            text: filtered[start..end].to_owned(),
        });
        start = end;
    }
}

fn map_logical_key(key: &Key) -> i32 {
    match key {
        Key::Named(NamedKey::Enter) => 1,
        Key::Named(NamedKey::Backspace) => 2,
        Key::Named(NamedKey::Tab) => 3,
        Key::Named(NamedKey::Escape) => 4,
        Key::Named(NamedKey::ArrowLeft) => 5,
        Key::Named(NamedKey::ArrowRight) => 6,
        Key::Named(NamedKey::ArrowUp) => 7,
        Key::Named(NamedKey::ArrowDown) => 8,
        Key::Named(NamedKey::Home) => 9,
        Key::Named(NamedKey::End) => 10,
        Key::Named(NamedKey::Delete) => 11,
        Key::Named(NamedKey::Space) => 12,
        Key::Named(NamedKey::PageUp) => 13,
        Key::Named(NamedKey::PageDown) => 14,
        Key::Named(NamedKey::F1) => 112,
        Key::Named(NamedKey::F2) => 113,
        Key::Named(NamedKey::F3) => 114,
        Key::Named(NamedKey::F4) => 115,
        Key::Named(NamedKey::F5) => 116,
        Key::Named(NamedKey::F6) => 117,
        Key::Named(NamedKey::F7) => 118,
        Key::Named(NamedKey::F8) => 119,
        Key::Named(NamedKey::F9) => 120,
        Key::Named(NamedKey::F10) => 121,
        Key::Named(NamedKey::F11) => 122,
        Key::Named(NamedKey::F12) => 123,
        Key::Character(s) => s
            .chars()
            .next()
            .filter(|c| c.is_ascii())
            .map(|c| c as i32)
            .unwrap_or(0),
        _ => 0,
    }
}
