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
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::device::Svga;
use crate::display::Frame;
use crate::log;
use crate::qmp::{ABS_MAX, Input, InputSink};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

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

/// Where the guest frame sits inside the window (letterboxed, aspect kept).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct View {
    x: f64,
    y: f64,
    scale: f64,
    frame_w: u32,
    frame_h: u32,
}

impl View {
    fn fit(window_w: u32, window_h: u32, frame_w: u32, frame_h: u32) -> Self {
        if frame_w == 0 || frame_h == 0 {
            return Self::default();
        }
        let scale = (f64::from(window_w) / f64::from(frame_w))
            .min(f64::from(window_h) / f64::from(frame_h));
        Self {
            x: (f64::from(window_w) - f64::from(frame_w) * scale) / 2.0,
            y: (f64::from(window_h) - f64::from(frame_h) * scale) / 2.0,
            scale,
            frame_w,
            frame_h,
        }
    }

    /// Absolute pointer coordinates (0..=ABS_MAX) of a window position.
    fn pointer(&self, position: PhysicalPosition<f64>) -> Option<(u32, u32)> {
        if self.scale <= 0.0 {
            return None;
        }
        let axis = |pos: f64, origin: f64, size: u32| {
            let pixel = ((pos - origin) / self.scale).clamp(0.0, f64::from(size.saturating_sub(1)));
            (pixel * f64::from(ABS_MAX) / f64::from(size.saturating_sub(1).max(1))).round() as u32
        };
        Some((
            axis(position.x, self.x, self.frame_w),
            axis(position.y, self.y, self.frame_h),
        ))
    }
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

    fn tick(&mut self) {
        {
            let mut state = self.svga.lock();
            self.svga.process_fifo(&mut state);
        }
        let shown = self.svga.capture(&mut self.frame).is_some();
        if let (Some(path), true) = (&self.snapshot, shown) {
            if self.last_snapshot.elapsed() >= Duration::from_secs(1) {
                self.last_snapshot = Instant::now();
                if let Err(error) = crate::save(path, &self.frame) {
                    log!("cannot write {}: {error}", path.display());
                }
            }
        }
        if let Some(window) = &self.window {
            window.request_redraw();
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
        if self.view.scale > 0.0 && self.frame.pixels.len() == self.frame.width * self.frame.height
        {
            let v = self.view;
            let x0 = v.x.max(0.0) as u32;
            let y0 = v.y.max(0.0) as u32;
            let x1 = ((v.x + f64::from(fw) * v.scale) as u32).min(size.width);
            let y1 = ((v.y + f64::from(fh) * v.scale) as u32).min(size.height);
            for wy in y0..y1 {
                let fy = (((f64::from(wy) - v.y) / v.scale) as usize).min(self.frame.height - 1);
                let src = &self.frame.pixels[fy * self.frame.width..(fy + 1) * self.frame.width];
                let dst = &mut buffer[(wy * size.width) as usize..((wy + 1) * size.width) as usize];
                for wx in x0..x1 {
                    let fx = (((f64::from(wx) - v.x) / v.scale) as usize).min(self.frame.width - 1);
                    dst[wx as usize] = src[fx];
                }
            }
        }
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
                log!("window closed");
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => self.draw(),
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
                if let Some((x, y)) = self.view.pointer(position) {
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
            self.tick();
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
    fn letterbox_and_pointer_mapping() {
        // 1280x800 guest in a 1920x1080 window: scaled 1.35, bars left/right.
        let view = View::fit(1920, 1080, 1280, 800);
        assert!((view.scale - 1.35).abs() < 1e-9);
        assert!((view.x - 96.0).abs() < 1e-9 && view.y.abs() < 1e-9);
        assert_eq!(view.pointer(PhysicalPosition::new(96.0, 0.0)), Some((0, 0)));
        assert_eq!(
            view.pointer(PhysicalPosition::new(5000.0, 5000.0)),
            Some((ABS_MAX, ABS_MAX))
        );
        assert_eq!(
            view.pointer(PhysicalPosition::new(0.0, -10.0)),
            Some((0, 0)),
            "clamped"
        );
        let (x, y) = view.pointer(PhysicalPosition::new(960.0, 540.0)).unwrap();
        assert!(
            (x as i64 - 16_383).abs() < 30 && (y as i64 - 16_383).abs() < 30,
            "centre"
        );
        assert_eq!(
            View::default().pointer(PhysicalPosition::new(1.0, 1.0)),
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
