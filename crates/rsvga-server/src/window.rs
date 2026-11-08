//! The rsvga window: shows the guest display and forwards keyboard and mouse
//! to the guest through QMP.
//!
//! Keys are sent by *physical position* (winit `KeyCode` -> QEMU qcode), so
//! a French AZERTY host and a French AZERTY guest agree on every key.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CustomCursor, Window, WindowId};

use crate::device::Svga;
use crate::display::Frame;
use crate::log;
use crate::qmp::{ABS_MAX, Input, InputSink};
use crate::scale::{View, render};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// How long the window size must stay put before the guest is asked to
/// follow it (a drag of the window edge sends one request, not hundreds).
const RESIZE_SETTLE: Duration = Duration::from_millis(300);
/// Smallest resolution asked of the guest.
const MIN_GUEST_SIZE: (u32, u32) = (800, 600);

/// QEMU qcode of a physical key.
fn qcode(code: KeyCode) -> Option<&'static str> {
    use KeyCode::*;
    Some(match code {
        KeyA => "a",
        KeyB => "b",
        KeyC => "c",
        KeyD => "d",
        KeyE => "e",
        KeyF => "f",
        KeyG => "g",
        KeyH => "h",
        KeyI => "i",
        KeyJ => "j",
        KeyK => "k",
        KeyL => "l",
        KeyM => "m",
        KeyN => "n",
        KeyO => "o",
        KeyP => "p",
        KeyQ => "q",
        KeyR => "r",
        KeyS => "s",
        KeyT => "t",
        KeyU => "u",
        KeyV => "v",
        KeyW => "w",
        KeyX => "x",
        KeyY => "y",
        KeyZ => "z",
        Digit0 => "0",
        Digit1 => "1",
        Digit2 => "2",
        Digit3 => "3",
        Digit4 => "4",
        Digit5 => "5",
        Digit6 => "6",
        Digit7 => "7",
        Digit8 => "8",
        Digit9 => "9",
        Escape => "esc",
        F1 => "f1",
        F2 => "f2",
        F3 => "f3",
        F4 => "f4",
        F5 => "f5",
        F6 => "f6",
        F7 => "f7",
        F8 => "f8",
        F9 => "f9",
        F10 => "f10",
        F11 => "f11",
        F12 => "f12",
        Backquote => "grave_accent",
        Minus => "minus",
        Equal => "equal",
        Backspace => "backspace",
        Tab => "tab",
        BracketLeft => "bracket_left",
        BracketRight => "bracket_right",
        Backslash => "backslash",
        CapsLock => "caps_lock",
        Semicolon => "semicolon",
        Quote => "apostrophe",
        Enter => "ret",
        IntlBackslash => "less",
        Comma => "comma",
        Period => "dot",
        Slash => "slash",
        Space => "spc",
        ShiftLeft => "shift",
        ShiftRight => "shift_r",
        ControlLeft => "ctrl",
        ControlRight => "ctrl_r",
        AltLeft => "alt",
        AltRight => "alt_r",
        SuperLeft => "meta_l",
        SuperRight => "meta_r",
        ContextMenu => "menu",
        Insert => "insert",
        Delete => "delete",
        Home => "home",
        End => "end",
        PageUp => "pgup",
        PageDown => "pgdn",
        ArrowUp => "up",
        ArrowDown => "down",
        ArrowLeft => "left",
        ArrowRight => "right",
        PrintScreen => "print",
        ScrollLock => "scroll_lock",
        Pause => "pause",
        NumLock => "num_lock",
        Numpad0 => "kp_0",
        Numpad1 => "kp_1",
        Numpad2 => "kp_2",
        Numpad3 => "kp_3",
        Numpad4 => "kp_4",
        Numpad5 => "kp_5",
        Numpad6 => "kp_6",
        Numpad7 => "kp_7",
        Numpad8 => "kp_8",
        Numpad9 => "kp_9",
        NumpadDecimal => "kp_decimal",
        NumpadAdd => "kp_add",
        NumpadSubtract => "kp_subtract",
        NumpadMultiply => "kp_multiply",
        NumpadDivide => "kp_divide",
        NumpadEnter => "kp_enter",
        _ => return None,
    })
}

/// Absolute pointer coordinates (0..=ABS_MAX) of a window position.
fn pointer(view: &View, position: PhysicalPosition<f64>) -> Option<(u32, u32)> {
    let (fx, fy) = view.frame_position(position.x, position.y)?;
    let axis = |pixel: f64, size: u32| {
        (pixel * f64::from(ABS_MAX) / f64::from(size.saturating_sub(1).max(1))).round() as u32
    };
    Some((axis(fx, view.frame_w), axis(fy, view.frame_h)))
}

struct App {
    svga: Arc<Svga>,
    snapshot: Option<PathBuf>,
    input: Option<InputSink>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    frame: Frame,
    view: View,
    pressed: HashSet<&'static str>,
    /// Guest resolution the window was last sized for.
    sized_for: (usize, usize),
    /// A window size the user chose that differs from the guest's, and
    /// when it was last changed.
    wanted: Option<(PhysicalSize<u32>, Instant)>,
    /// Resolution last asked of the guest.
    asked: Option<(u32, u32)>,
    /// Guest cursor definition last applied, and its visibility.
    cursor_serial: u64,
    cursor_visible: bool,
    /// The user closed the window once: the guest is shutting down.
    closing: bool,
    cursor_logged: bool,
    next_tick: Instant,
    last_snapshot: Instant,
}

impl App {
    fn send(&self, input: Input) {
        if let Some(sink) = &self.input {
            sink.send(input);
        }
    }

    fn release_all(&mut self) {
        for qcode in std::mem::take(&mut self.pressed) {
            self.send(Input::Key { qcode, down: false });
        }
    }

    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        if self.svga.client_gone() {
            log!("the VM stopped: closing the window");
            event_loop.exit();
            return;
        }
        {
            let mut state = self.svga.lock();
            self.svga.process_fifo(&mut state);
            self.svga.watch(&mut state);
        }
        let shown = self.svga.capture(&mut self.frame);
        let changed = shown.is_some_and(|s| s.changed);
        if let (Some(path), true) = (&self.snapshot, changed) {
            if self.last_snapshot.elapsed() >= Duration::from_secs(1) {
                self.last_snapshot = Instant::now();
                if let Err(error) = crate::save(path, &self.frame) {
                    log!("cannot write {}: {error}", path.display());
                }
            }
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        // Follow the guest resolution so it is shown pixel for pixel.
        let size = (self.frame.width, self.frame.height);
        if shown.is_some() && size != self.sized_for && size.0 > 0 && size.1 > 0 {
            self.sized_for = size;
            let _ = window.request_inner_size(PhysicalSize::new(size.0 as u32, size.1 as u32));
        }
        if changed {
            window.request_redraw();
        }
        self.ask_guest_size(shown.is_some());
        self.update_cursor(event_loop, &window);
    }

    /// Asks the guest (through the VMware Tools) to take the window size
    /// once the user has stopped resizing it.
    fn ask_guest_size(&mut self, showing: bool) {
        let Some((size, since)) = self.wanted else {
            return;
        };
        if !showing || since.elapsed() < RESIZE_SETTLE {
            return;
        }
        self.wanted = None;
        let (max_w, max_h) = {
            let state = self.svga.lock();
            let config = state.regs.config();
            (config.max_width, config.max_height)
        };
        let width = size.width.clamp(MIN_GUEST_SIZE.0, max_w);
        let height = size.height.clamp(MIN_GUEST_SIZE.1, max_h);
        let current = (self.frame.width as u32, self.frame.height as u32);
        if (width, height) == current || Some((width, height)) == self.asked {
            return;
        }
        self.asked = Some((width, height));
        if let Some(input) = &self.input {
            input.set_resolution(width, height);
        }
    }

    /// Shows the guest's cursor as the window's cursor: no latency, and the
    /// shape follows the guest (arrow, text, resize...).
    fn update_cursor(&mut self, event_loop: &ActiveEventLoop, window: &Window) {
        let (serial, image, visible) = self.svga.cursor();
        if serial != self.cursor_serial {
            self.cursor_serial = serial;
            if let Some(image) = image {
                let source = CustomCursor::from_rgba(
                    image.rgba.clone(),
                    image.width,
                    image.height,
                    image.hot_x,
                    image.hot_y,
                );
                match source {
                    Ok(source) => {
                        window.set_cursor(event_loop.create_custom_cursor(source));
                        if !self.cursor_logged {
                            self.cursor_logged = true;
                            log!("window now shows the guest cursor");
                        }
                    }
                    Err(error) => log!("guest cursor rejected: {error}"),
                }
            }
        }
        if visible != self.cursor_visible {
            log!("guest cursor {}", if visible { "shown" } else { "hidden" });
            self.cursor_visible = visible;
            window.set_cursor_visible(visible);
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        buffer.fill(0);
        let (fw, fh) = (self.frame.width as u32, self.frame.height as u32);
        self.view = View::fit(size.width, size.height, fw, fh);
        render(&self.view, &self.frame.pixels, &mut buffer, size.width);
        let _ = buffer.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("rsvga")
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Rc::new(window),
            Err(error) => {
                log!("cannot open a window: {error}");
                event_loop.exit();
                return;
            }
        };
        let surface = softbuffer::Context::new(window.clone())
            .and_then(|context| softbuffer::Surface::new(&context, window.clone()));
        match surface {
            Ok(surface) => self.surface = Some(surface),
            Err(error) => {
                log!("cannot draw into the window: {error}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.release_all();
                match (&self.input, self.closing) {
                    // First close: shut the guest down cleanly; the window
                    // closes by itself once QEMU stops.
                    (Some(input), false) if !self.svga.client_gone() => {
                        self.closing = true;
                        input.powerdown();
                        if let Some(window) = &self.window {
                            window.set_title(
                                "rsvga - Windows is shutting down (close again to force)",
                            );
                        }
                    }
                    _ => {
                        log!("window closed");
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(size) => {
                // Sizes rsvga set itself (the initial one, those matching
                // the guest) are not requests from the user.
                let guest = (self.frame.width as u32, self.frame.height as u32);
                let user = self.sized_for != (0, 0) && (size.width, size.height) != guest;
                self.wanted = user.then(|| (size, Instant::now()));
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::Focused(false) => self.release_all(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let Some(qcode) = qcode(code) else { return };
                let down = event.state == ElementState::Pressed;
                // The guest generates its own auto-repeat from the held key.
                if event.repeat {
                    return;
                }
                if down {
                    self.pressed.insert(qcode);
                } else {
                    self.pressed.remove(qcode);
                }
                self.send(Input::Key { qcode, down });
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some((x, y)) = pointer(&self.view, position) {
                    self.send(Input::Move { x, y });
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => "left",
                    MouseButton::Right => "right",
                    MouseButton::Middle => "middle",
                    MouseButton::Back => "side",
                    MouseButton::Forward => "extra",
                    MouseButton::Other(_) => return,
                };
                self.send(Input::Button {
                    button,
                    down: state == ElementState::Pressed,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if dy != 0.0 {
                    let button = if dy > 0.0 { "wheel-up" } else { "wheel-down" };
                    self.send(Input::Button { button, down: true });
                    self.send(Input::Button {
                        button,
                        down: false,
                    });
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_tick {
            self.tick(event_loop);
            self.next_tick = now + FRAME_INTERVAL;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }
}

/// Runs the window on the current (main) thread until it is closed.
pub fn run(svga: Arc<Svga>, snapshot: Option<PathBuf>, qmp: Option<PathBuf>) {
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            log!("no display available for a window: {error}");
            std::process::exit(1);
        }
    };
    let mut app = App {
        svga,
        snapshot,
        input: qmp.map(InputSink::spawn),
        window: None,
        surface: None,
        frame: Frame::default(),
        view: View::default(),
        pressed: HashSet::new(),
        sized_for: (0, 0),
        wanted: None,
        asked: None,
        cursor_serial: 0,
        cursor_visible: true,
        closing: false,
        cursor_logged: false,
        next_tick: Instant::now(),
        last_snapshot: Instant::now(),
    };
    if let Err(error) = event_loop.run_app(&mut app) {
        log!("window error: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_covers_the_whole_guest_screen() {
        let view = View::fit(2114, 1157, 1920, 1080);
        assert_eq!(
            pointer(&view, PhysicalPosition::new(97.0, 38.0)),
            Some((0, 0))
        );
        assert_eq!(
            pointer(&view, PhysicalPosition::new(5000.0, 5000.0)),
            Some((ABS_MAX, ABS_MAX))
        );
        let (x, y) = pointer(&view, PhysicalPosition::new(97.0 + 960.0, 38.0 + 540.0)).unwrap();
        assert!((i64::from(x) - 16_383).abs() < 30 && (i64::from(y) - 16_383).abs() < 30);
        assert_eq!(
            pointer(&View::default(), PhysicalPosition::new(1.0, 1.0)),
            None
        );
    }

    #[test]
    fn physical_keys_map_to_qcodes() {
        assert_eq!(
            qcode(KeyCode::KeyQ),
            Some("q"),
            "AZERTY 'A' is the physical Q key"
        );
        assert_eq!(
            qcode(KeyCode::IntlBackslash),
            Some("less"),
            "the <> key of ISO keyboards"
        );
        assert_eq!(qcode(KeyCode::AltRight), Some("alt_r"), "AltGr");
        assert_eq!(qcode(KeyCode::Fn), None);
    }
}
