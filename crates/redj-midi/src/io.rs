//! Hardware MIDI through midir.

use std::sync::mpsc::Sender;

use crate::mapper::MidiMessage;

#[derive(Debug, thiserror::Error)]
pub enum MidiIoError {
    #[error("MIDI backend error: {0}")]
    Backend(String),
    #[error("MIDI port not found: {0}")]
    NotFound(String),
}

fn backend(e: impl std::fmt::Display) -> MidiIoError {
    MidiIoError::Backend(e.to_string())
}

/// Names of the MIDI input ports.
pub fn input_ports() -> Vec<String> {
    let Ok(m) = midir::MidiInput::new("redj-list") else {
        return Vec::new();
    };
    m.ports()
        .iter()
        .filter_map(|p| m.port_name(p).ok())
        .collect()
}

/// Names of the MIDI output ports.
pub fn output_ports() -> Vec<String> {
    let Ok(m) = midir::MidiOutput::new("redj-list") else {
        return Vec::new();
    };
    m.ports()
        .iter()
        .filter_map(|p| m.port_name(p).ok())
        .collect()
}

/// An open input; incoming messages are sent to the channel given to
/// [`MidiInputConn::open`]. Dropping it closes the port.
pub struct MidiInputConn {
    _conn: midir::MidiInputConnection<()>,
    pub port: String,
}

impl std::fmt::Debug for MidiInputConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiInputConn")
            .field("port", &self.port)
            .finish()
    }
}

impl MidiInputConn {
    pub fn open(port_name: &str, tx: Sender<MidiMessage>) -> Result<Self, MidiIoError> {
        let mut input = midir::MidiInput::new("redj").map_err(backend)?;
        input.ignore(midir::Ignore::All);
        let port = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).ok().as_deref() == Some(port_name))
            .ok_or_else(|| MidiIoError::NotFound(port_name.to_owned()))?;
        let conn = input
            .connect(
                &port,
                "redj-in",
                move |_, bytes, _| {
                    if let Some(m) = MidiMessage::from_bytes(bytes) {
                        let _ = tx.send(m);
                    }
                },
                (),
            )
            .map_err(backend)?;
        Ok(Self {
            _conn: conn,
            port: port_name.to_owned(),
        })
    }
}

/// An open output (LED feedback).
pub struct MidiOutputConn {
    conn: midir::MidiOutputConnection,
    pub port: String,
}

impl std::fmt::Debug for MidiOutputConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiOutputConn")
            .field("port", &self.port)
            .finish()
    }
}

impl MidiOutputConn {
    pub fn open(port_name: &str) -> Result<Self, MidiIoError> {
        let output = midir::MidiOutput::new("redj").map_err(backend)?;
        let port = output
            .ports()
            .into_iter()
            .find(|p| output.port_name(p).ok().as_deref() == Some(port_name))
            .ok_or_else(|| MidiIoError::NotFound(port_name.to_owned()))?;
        let conn = output.connect(&port, "redj-out").map_err(backend)?;
        Ok(Self {
            conn,
            port: port_name.to_owned(),
        })
    }

    pub fn send(&mut self, m: MidiMessage) {
        let _ = self.conn.send(&m.to_bytes());
    }
}
