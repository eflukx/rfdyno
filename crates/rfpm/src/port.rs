//! Finding, reading and commanding the meter's serial port.

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serialport::{SerialPort, SerialPortType};

use crate::command::{COMMAND_SPACING, Command, CommandError};
use crate::parse::{Event, Parser};

/// The meter's USB vendor id (ST's, shared with every STM32 USB serial example).
pub const VID: u16 = 0x0483;

/// The meter's USB product id (ST's virtual COM port).
pub const PID: u16 = 0x5740;

/// The baud rate the vendor app uses. This meter is native USB, where it doesn't matter, but
/// boards with a USB-UART bridge need it.
const BAUD: u32 = 460_800;

/// How long a read waits for data before giving up (and returning nothing).
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// A serial port that looks like the meter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterPort {
    /// The port's name: `COM50`, `/dev/ttyACM0`, `/dev/cu.usbmodem…`.
    pub name: String,
    /// The USB serial number, if the system reports one.
    pub serial_number: Option<String>,
    /// The USB interface the port belongs to, if the system reports it.
    pub interface: Option<u8>,
}

/// Lists the ports with the meter's USB ids, the data interface first (with Windows' own
/// driver on the split device only that one opens). Any STM32 USB serial example has these
/// ids too, so a port may not be a meter.
pub fn find_ports() -> serialport::Result<Vec<MeterPort>> {
    let mut ports: Vec<MeterPort> = serialport::available_ports()?
        .into_iter()
        .filter_map(|port| match port.port_type {
            SerialPortType::UsbPort(usb) if usb.vid == VID && usb.pid == PID => Some(MeterPort {
                name: port.port_name,
                serial_number: usb.serial_number,
                interface: usb.interface,
            }),
            _ => None,
        })
        .collect();
    ports.sort_by_key(|port| (port.interface == Some(0), port.name.clone()));
    Ok(ports)
}

/// Why sending a command failed.
#[derive(Debug)]
pub enum SendError {
    /// The command can't be written (a value out of range).
    Command(CommandError),
    /// The port failed.
    Port(io::Error),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Command(error) => error.fmt(f),
            Self::Port(error) => write!(f, "can't write to the meter: {error}"),
        }
    }
}

impl std::error::Error for SendError {}

/// Sends commands to the meter; cheap to clone, usable from another thread than the reader.
#[derive(Clone)]
pub struct Commander {
    port: Arc<Mutex<Box<dyn SerialPort>>>,
    last_sent: Arc<Mutex<Option<Instant>>>,
}

impl Commander {
    /// Sends `command` in one write, waiting first if the last one went out less than
    /// [`COMMAND_SPACING`] ago. Blocks for at most that long.
    pub fn send(&self, command: Command) -> Result<(), SendError> {
        let bytes = command.bytes().map_err(SendError::Command)?;
        let mut last_sent = self
            .last_sent
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(wait) = last_sent.and_then(|at| COMMAND_SPACING.checked_sub(at.elapsed())) {
            thread::sleep(wait);
        }
        let mut port = self
            .port
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        port.write_all(&bytes)
            .and_then(|()| port.flush())
            .map_err(SendError::Port)?;
        *last_sent = Some(Instant::now());
        Ok(())
    }
}

/// An open meter.
pub struct Meter {
    port: Box<dyn SerialPort>,
    name: String,
    parser: Parser,
    buffer: Vec<u8>,
    commander: Commander,
}

impl Meter {
    /// Opens the meter on the port `name`, at 460800 8N1 with DTR and RTS low (as the vendor
    /// app does). Sends nothing: the meter streams by itself.
    pub fn open(name: &str) -> serialport::Result<Self> {
        let mut port = serialport::new(name, BAUD).timeout(READ_TIMEOUT).open()?;
        port.write_data_terminal_ready(false)?;
        port.write_request_to_send(false)?;
        let commander = Commander {
            port: Arc::new(Mutex::new(port.try_clone()?)),
            last_sent: Arc::new(Mutex::new(None)),
        };
        Ok(Self {
            port,
            name: name.to_owned(),
            parser: Parser::new(),
            buffer: vec![0; 4096],
            commander,
        })
    }

    /// Opens the first of [`find_ports`] that opens.
    pub fn open_first() -> serialport::Result<Self> {
        let mut last_error = None;
        for port in find_ports()? {
            match Self::open(&port.name) {
                Ok(meter) => return Ok(meter),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            serialport::Error::new(
                serialport::ErrorKind::NoDevice,
                format!("no RF power meter (USB {VID:04x}:{PID:04x}) found"),
            )
        }))
    }

    /// The port's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A handle to send commands with, also from another thread.
    pub fn commander(&self) -> Commander {
        self.commander.clone()
    }

    /// Sends `command` (see [`Commander::send`]).
    pub fn send(&self, command: Command) -> Result<(), SendError> {
        self.commander.send(command)
    }

    /// Reads what has arrived (waiting up to 200 ms for something), calling `emit` for each
    /// event. Returns the number of bytes read; 0 when nothing came in time.
    pub fn read(&mut self, emit: impl FnMut(Event)) -> serialport::Result<usize> {
        match self.port.read(&mut self.buffer) {
            Ok(count) => {
                self.parser.feed(&self.buffer[..count], emit);
                Ok(count)
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(error) => Err(error.into()),
        }
    }
}
