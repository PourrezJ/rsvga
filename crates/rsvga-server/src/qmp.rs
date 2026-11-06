//! Keyboard and mouse injection into the guest through QEMU's QMP socket
//! (`input-send-event`). Keys are QEMU "qcodes", i.e. physical key
//! positions, so the guest applies its own keyboard layout.
//!
//! A background thread owns the socket: the UI only queues events, and a
//! slow or absent QEMU never blocks the window. Pointer moves are coalesced.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::log;

/// Largest coordinate of QEMU's absolute pointer axes.
pub const ABS_MAX: u32 = 0x7fff;

/// One input event for the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A key, by QEMU qcode name.
    Key {
        /// qcode, e.g. `"a"`, `"shift"`, `"ret"`.
        qcode: &'static str,
        /// Pressed (true) or released.
        down: bool,
    },
    /// Absolute pointer position, each axis in `0..=ABS_MAX`.
    Move {
        /// Horizontal position.
        x: u32,
        /// Vertical position.
        y: u32,
    },
    /// A pointer button: `"left"`, `"right"`, `"middle"`, `"wheel-up"`...
    Button {
        /// QEMU button name.
        button: &'static str,
        /// Pressed (true) or released.
        down: bool,
    },
}

/// JSON object of one event in an `input-send-event` command. Every string
/// is a fixed ASCII name from this crate, so no escaping is needed.
fn event_json(input: &Input) -> String {
    match *input {
        Input::Key { qcode, down } => format!(
            r#"{{"type":"key","data":{{"down":{down},"key":{{"type":"qcode","data":"{qcode}"}}}}}}"#
        ),
        Input::Move { x, y } => format!(
            r#"{{"type":"abs","data":{{"axis":"x","value":{}}}}},{{"type":"abs","data":{{"axis":"y","value":{}}}}}"#,
            x.min(ABS_MAX),
            y.min(ABS_MAX)
        ),
        Input::Button { button, down } => {
            format!(r#"{{"type":"btn","data":{{"down":{down},"button":"{button}"}}}}"#)
        }
    }
}

/// The whole `input-send-event` command for a batch of events.
fn command_json(batch: &[Input]) -> String {
    let events: Vec<String> = batch.iter().map(event_json).collect();
    format!(
        r#"{{"execute":"input-send-event","arguments":{{"events":[{}]}}}}"#,
        events.join(",")
    )
}

/// Merges consecutive pointer moves: only the last position matters.
fn coalesce(events: Vec<Input>) -> Vec<Input> {
    let mut out: Vec<Input> = Vec::with_capacity(events.len());
    for event in events {
        if let (Some(Input::Move { .. }), Input::Move { .. }) = (out.last(), event) {
            out.pop();
        }
        out.push(event);
    }
    out
}

/// Queue of input events, sent by a background thread.
#[derive(Clone)]
pub struct InputSink {
    tx: Sender<Input>,
}

impl InputSink {
    /// Starts the sender thread for the QMP socket at `path`. The socket is
    /// (re)connected lazily, so QEMU may start after rsvga.
    pub fn spawn(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || sender(&path, &rx));
        Self { tx }
    }

    /// Queues an event; never blocks.
    pub fn send(&self, input: Input) {
        let _ = self.tx.send(input);
    }
}

struct Connection {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Connection {
    fn open(path: &PathBuf) -> std::io::Result<Self> {
        let writer = UnixStream::connect(path)?;
        writer.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut conn = Self {
            reader: BufReader::new(writer.try_clone()?),
            writer,
        };
        let mut greeting = String::new();
        conn.reader.read_line(&mut greeting)?;
        conn.command(r#"{"execute":"qmp_capabilities"}"#)?;
        Ok(conn)
    }

    /// Sends a command and waits for its reply, skipping asynchronous events.
    fn command(&mut self, json: &str) -> std::io::Result<()> {
        self.writer.write_all(json.as_bytes())?;
        self.writer.write_all(b"\n")?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(std::io::Error::other("QMP closed"));
            }
            if line.contains("\"return\"") {
                return Ok(());
            }
            if line.contains("\"error\"") {
                return Err(std::io::Error::other(line.trim().to_string()));
            }
        }
    }
}

fn sender(path: &PathBuf, rx: &Receiver<Input>) {
    let mut conn: Option<Connection> = None;
    let mut warned = false;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        let batch = coalesce(batch);
        if conn.is_none() {
            match Connection::open(path) {
                Ok(c) => {
                    log!("QMP input connected ({})", path.display());
                    conn = Some(c);
                    warned = false;
                }
                Err(error) => {
                    if !warned {
                        log!("QMP input unavailable at {}: {error}", path.display());
                        warned = true;
                    }
                    continue; // drop these events; retry on the next ones
                }
            }
        }
        if let Some(c) = conn.as_mut() {
            if let Err(error) = c.command(&command_json(&batch)) {
                log!("QMP input error: {error}");
                conn = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_events() {
        assert_eq!(
            event_json(&Input::Key {
                qcode: "a",
                down: true
            }),
            r#"{"type":"key","data":{"down":true,"key":{"type":"qcode","data":"a"}}}"#
        );
        assert_eq!(
            event_json(&Input::Move { x: 1, y: 99_999 }),
            r#"{"type":"abs","data":{"axis":"x","value":1}},{"type":"abs","data":{"axis":"y","value":32767}}"#
        );
        let command = command_json(&[Input::Button {
            button: "left",
            down: false,
        }]);
        assert!(
            command.starts_with(
                r#"{"execute":"input-send-event","arguments":{"events":[{"type":"btn""#
            )
        );
        assert!(command.ends_with("]}}"));
    }

    #[test]
    fn moves_are_coalesced_but_keys_are_not() {
        let key = Input::Key {
            qcode: "a",
            down: true,
        };
        let events = vec![
            Input::Move { x: 1, y: 1 },
            Input::Move { x: 2, y: 2 },
            key,
            Input::Move { x: 3, y: 3 },
            Input::Move { x: 4, y: 4 },
        ];
        assert_eq!(
            coalesce(events),
            vec![Input::Move { x: 2, y: 2 }, key, Input::Move { x: 4, y: 4 }]
        );
    }

    #[test]
    fn talks_qmp_to_a_fake_qemu() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("rsvga-qmp-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let listener = UnixListener::bind(&dir).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            writer.write_all(b"{\"QMP\": {}}\n").unwrap();
            let mut lines = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                writer
                    .write_all(b"{\"event\": \"NOISE\"}\n{\"return\": {}}\n")
                    .unwrap();
                lines.push(line);
            }
            lines
        });
        let sink = InputSink::spawn(dir.clone());
        sink.send(Input::Key {
            qcode: "ret",
            down: true,
        });
        let lines = server.join().unwrap();
        assert!(lines[0].contains("qmp_capabilities"));
        assert!(lines[1].contains(r#""data":"ret""#));
        let _ = std::fs::remove_file(&dir);
    }
}
