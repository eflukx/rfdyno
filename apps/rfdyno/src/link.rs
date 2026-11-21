//! The link to the meter: a reader thread that owns the port, and commands on threads of their
//! own so the meter's command spacing never blocks the UI.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rfpm::{Command, Commander, Event, Frame, Meter, SampleRate, Settings};

/// How long the meter pauses after a setting, before it takes the next command.
pub const SETTLE: Duration = Duration::from_millis(1200);

/// How long to wait for a settings reply before asking once more.
pub const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);

/// What the reader and command threads send.
pub enum Message {
    /// A block, and when it came in (taken on the reader thread, for the rate check).
    Frame(Frame, Instant),
    Settings(Settings),
    /// The sample rate command went out: the meter samples at this rate from now on.
    RateSet(SampleRate),
    /// The settings went out; a reply should follow.
    Sent,
    /// A command failed.
    Error(String),
    /// The port is gone.
    Lost(String),
}

/// Where setting the meter stands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Applying {
    Idle,
    /// Commands are on their way.
    Sending,
    /// Sent; waiting for the reply since then, asked once more already or not.
    Waiting {
        since: Instant,
        retried: bool,
    },
}

/// A running reader thread; dropping it stops the thread (within one read timeout).
pub struct Link {
    /// The port's name.
    pub name: String,
    /// What the threads sent.
    pub messages: Receiver<Message>,
    sender: Sender<Message>,
    commander: Commander,
    stop: Arc<AtomicBool>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Link {
    /// Starts reading `meter` on a thread of its own, repainting `ctx` when something comes in.
    pub fn start(mut meter: Meter, ctx: &egui::Context) -> Self {
        let (sender, messages) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let name = meter.name().to_owned();
        let commander = meter.commander();

        let ctx = ctx.clone();
        let thread_sender = sender.clone();
        let thread_stop = stop.clone();
        thread::spawn(move || {
            let mut collector = rfpm::FrameCollector::new();
            while !thread_stop.load(Ordering::Relaxed) {
                let mut messages = Vec::new();
                let read = meter.read(|event| {
                    if let Event::Settings(settings) = event {
                        messages.push(Message::Settings(settings));
                    }
                    let frame = collector.push(event);
                    messages.extend(frame.map(|frame| Message::Frame(frame, Instant::now())));
                });
                if let Err(error) = read {
                    messages.push(Message::Lost(error.to_string()));
                }
                let lost = matches!(messages.last(), Some(Message::Lost(_)));
                for message in messages {
                    if thread_sender.send(message).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
                if lost {
                    return;
                }
            }
        });

        Self {
            name,
            messages,
            sender,
            commander,
            stop,
        }
    }

    /// Sets the meter (sample rate, then frequency and attenuation) and asks for the settings
    /// back, on a thread of its own.
    pub fn apply(&self, ctx: &egui::Context, settings: Settings, rate: SampleRate) {
        let commander = self.commander.clone();
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let sent = commander
                .send(Command::SampleRate(rate))
                .inspect(|()| {
                    let _ = sender.send(Message::RateSet(rate));
                    ctx.request_repaint();
                })
                .and_then(|()| {
                    thread::sleep(SETTLE);
                    commander.send(Command::Set(settings))
                })
                .and_then(|()| {
                    thread::sleep(SETTLE);
                    commander.send(Command::Read)
                });
            let _ = sender.send(match sent {
                Ok(()) => Message::Sent,
                Err(error) => Message::Error(error.to_string()),
            });
            ctx.request_repaint();
        });
    }

    /// Asks for the settings again.
    pub fn read_settings(&self) {
        let commander = self.commander.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            if let Err(error) = commander.send(Command::Read) {
                let _ = sender.send(Message::Error(error.to_string()));
            }
        });
    }
}
