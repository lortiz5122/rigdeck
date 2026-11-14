//! The engine: one thread that owns the transport and the driver, sends one command at a time,
//! matches replies, polls in the background and keeps the radio state.
//!
//! Rules it enforces:
//! - one command outstanding at a time, so nothing is sent while a TX command is pending;
//! - user requests go before background polls;
//! - a poll the radio refuses (NG) backs off (1 s, 2 s, 4 s … 60 s) instead of repeating every
//!   second; the backoff resets when the mode changes, since refusals depend on the mode;
//! - transmit has a watchdog: the engine unkeys after the time the caller allowed;
//! - shutting down unkeys first if the engine keyed the radio.

use crate::protocol::{Expect, Incoming, Protocol};
use crate::rig::{Capabilities, Event, ModeSetting, Request};
use crate::scope::Sweep;
use crate::transport::Transport;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// The radio said OK.
    Ack,
    /// The radio answered with data (events decoded from it).
    Data(Vec<Event>),
    /// Sent; this radio does not answer that command.
    Sent,
}

impl Reply {
    pub fn first_event(&self) -> Option<&Event> {
        match self {
            Reply::Data(e) => e.first(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    Unsupported(&'static str),
    Refused(&'static str),
    Timeout(&'static str),
    Io(String),
    Stopped,
    Mismatch(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Unsupported(n) => write!(f, "{n}: not supported by this radio"),
            EngineError::Refused(n) => write!(f, "{n}: refused by the radio (NG)"),
            EngineError::Timeout(n) => write!(f, "{n}: no answer from the radio"),
            EngineError::Io(e) => write!(f, "link error: {e}"),
            EngineError::Stopped => write!(f, "not connected"),
            EngineError::Mismatch(m) => write!(f, "radio did not confirm: {m}"),
        }
    }
}
impl std::error::Error for EngineError {}

pub type EngineResult<T> = Result<T, EngineError>;

type ReplyTo = Option<Sender<EngineResult<Reply>>>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineStats {
    pub sent: u64,
    pub messages: u64,
    pub refused: u64,
    pub timeouts: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub sweeps: u64,
    pub sweeps_dropped: u64,
    /// Polls currently backed off after an NG, with their current interval.
    pub backed_off: Vec<(&'static str, Duration)>,
    /// Refusals per command name.
    pub refused_by_command: HashMap<&'static str, u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RadioState {
    pub connected: bool,
    pub link_error: Option<String>,
    pub id: Option<u32>,
    pub frequency_hz: Option<u64>,
    pub mode: Option<ModeSetting>,
    pub s_meter: Option<u16>,
    pub power_pct: Option<u16>,
    pub po: Option<u16>,
    pub swr: Option<u16>,
    pub alc: Option<u16>,
    pub tx: bool,
    pub scope_on: Option<bool>,
    pub stats: EngineStats,
}

#[derive(Debug, Clone)]
pub struct PollItem {
    pub request: Request,
    pub interval: Duration,
    /// Only poll while transmitting (power/SWR/ALC meters).
    pub tx_only: bool,
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub reply_timeout: Duration,
    pub polls: Vec<PollItem>,
    /// Longest transmission the engine allows when the caller does not say.
    pub default_tx_limit: Duration,
    /// Ask the radio to push changes (Yaesu AI1). Icom transceive is a radio menu item.
    pub auto_information: bool,
    pub max_backoff: Duration,
}

impl EngineConfig {
    pub fn default_for(caps: &Capabilities) -> Self {
        let p = |request, ms, tx_only| PollItem { request, interval: Duration::from_millis(ms), tx_only };
        let mut polls = vec![
            p(Request::ReadSMeter, 200, false),
            p(Request::ReadFrequency, 1000, false),
            p(Request::ReadMode, 2000, false),
            p(Request::ReadTx, 1000, false),
            p(Request::ReadPowerLevel, 5000, false),
            p(Request::ReadPo, 250, true),
            p(Request::ReadSwr, 500, true),
            p(Request::ReadAlc, 500, true),
        ];
        if caps.has_transceive {
            // The radio pushes frequency and mode changes; poll them only as a safety net.
            for item in &mut polls {
                if matches!(item.request, Request::ReadFrequency | Request::ReadMode) {
                    item.interval *= 3;
                }
            }
        }
        Self {
            reply_timeout: Duration::from_millis(500),
            polls,
            default_tx_limit: Duration::from_secs(120),
            auto_information: true,
            max_backoff: Duration::from_secs(60),
        }
    }

    pub fn without_polling(mut self) -> Self {
        self.polls.clear();
        self
    }
}

enum Cmd {
    Request { req: Request, reply: Sender<EngineResult<Reply>>, tx_limit: Option<Duration> },
    Shutdown,
}

struct Outstanding {
    name: &'static str,
    expect: Expect,
    deadline: Instant,
    reply: Option<Sender<EngineResult<Reply>>>,
    poll_index: Option<usize>,
    /// Some(on) for a transmit command, applied to the state when the radio confirms.
    tx_change: Option<bool>,
}

struct PollState {
    item: PollItem,
    next_due: Instant,
    backoff: Option<Duration>,
}

/// Handle to a running engine. Dropping it disconnects.
pub struct Engine {
    cmd: Mutex<Sender<Cmd>>,
    state: Arc<Mutex<RadioState>>,
    subscribers: Arc<Mutex<Vec<Sender<Event>>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    caps: Capabilities,
    reply_timeout: Duration,
    link: String,
}

impl Engine {
    pub fn start(transport: Box<dyn Transport>, protocol: Box<dyn Protocol>, config: EngineConfig) -> Engine {
        let caps = protocol.capabilities();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(RadioState { connected: true, ..Default::default() }));
        let subscribers: Arc<Mutex<Vec<Sender<Event>>>> = Arc::default();
        let link = transport.describe();
        let reply_timeout = config.reply_timeout;
        let worker = Worker::new(transport, protocol, config, cmd_rx, state.clone(), subscribers.clone());
        let thread = std::thread::Builder::new().name("rigdeck-engine".into()).spawn(move || worker.run()).expect("spawn engine thread");
        Engine { cmd: Mutex::new(cmd_tx), state, subscribers, thread: Mutex::new(Some(thread)), caps, reply_timeout, link }
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    pub fn link(&self) -> &str {
        &self.link
    }

    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap().connected
    }

    pub fn state(&self) -> RadioState {
        self.state.lock().unwrap().clone()
    }

    /// Receive every event (including scope sweeps) from now on.
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    fn send(&self, req: Request, tx_limit: Option<Duration>) -> EngineResult<Reply> {
        let (tx, rx) = mpsc::channel();
        self.cmd.lock().unwrap().send(Cmd::Request { req, reply: tx, tx_limit }).map_err(|_| EngineError::Stopped)?;
        // Queue wait plus the radio's own timeout; generous so a busy scope link does not fail requests.
        rx.recv_timeout(self.reply_timeout * 10 + Duration::from_secs(2)).map_err(|_| EngineError::Stopped)?
    }

    /// Send any request except transmit (use [`Engine::transmit`]).
    pub fn request(&self, req: Request) -> EngineResult<Reply> {
        if req.keys_transmitter() {
            return Err(EngineError::Unsupported("use transmit() to key the radio"));
        }
        self.send(req, None)
    }

    pub fn read_id(&self) -> EngineResult<u32> {
        match self.request(Request::ReadId)?.first_event() {
            Some(Event::Id(id)) => Ok(*id),
            other => Err(EngineError::Mismatch(format!("unexpected ID answer {other:?}"))),
        }
    }

    pub fn read_frequency(&self) -> EngineResult<u64> {
        match self.request(Request::ReadFrequency)?.first_event() {
            Some(Event::Frequency(f)) => Ok(*f),
            other => Err(EngineError::Mismatch(format!("unexpected frequency answer {other:?}"))),
        }
    }

    pub fn read_mode(&self) -> EngineResult<ModeSetting> {
        match self.request(Request::ReadMode)?.first_event() {
            Some(Event::Mode(m)) => Ok(*m),
            other => Err(EngineError::Mismatch(format!("unexpected mode answer {other:?}"))),
        }
    }

    pub fn read_s_meter(&self) -> EngineResult<u16> {
        match self.request(Request::ReadSMeter)?.first_event() {
            Some(Event::SMeter(v)) => Ok(*v),
            other => Err(EngineError::Mismatch(format!("unexpected S-meter answer {other:?}"))),
        }
    }

    /// Set the frequency and confirm it by reading it back.
    pub fn set_frequency(&self, hz: u64) -> EngineResult<u64> {
        if hz < self.caps.min_freq_hz || hz > self.caps.max_freq_hz {
            return Err(EngineError::Unsupported("frequency outside the radio's range"));
        }
        self.request(Request::SetFrequency(hz))?;
        let got = self.read_frequency()?;
        if got != hz {
            return Err(EngineError::Mismatch(format!("asked for {hz} Hz, radio reports {got} Hz")));
        }
        Ok(got)
    }

    /// Set the mode and confirm it by reading it back.
    pub fn set_mode(&self, m: ModeSetting) -> EngineResult<ModeSetting> {
        self.request(Request::SetMode(m))?;
        let got = self.read_mode()?;
        if got.mode != m.mode || got.data != m.data || (m.filter.is_some() && got.filter.is_some() && got.filter != m.filter) {
            return Err(EngineError::Mismatch(format!("asked for {}, radio reports {}", m.label(), got.label())));
        }
        Ok(got)
    }

    pub fn set_power(&self, pct: u16) -> EngineResult<u16> {
        self.request(Request::SetPowerLevel(pct))?;
        match self.request(Request::ReadPowerLevel)?.first_event() {
            Some(Event::PowerLevel(p)) => Ok(*p),
            other => Err(EngineError::Mismatch(format!("unexpected power answer {other:?}"))),
        }
    }

    /// Key or unkey the transmitter. Keying needs a limit; the engine unkeys when it runs out.
    pub fn transmit(&self, on: bool, limit: Duration) -> EngineResult<Reply> {
        self.send(Request::SetTx(on), on.then_some(limit))
    }

    pub fn read_memory(&self, n: u16) -> EngineResult<crate::memory::MemoryChannel> {
        match self.request(Request::ReadMemory(n)) {
            Ok(r) => match r.first_event() {
                Some(Event::Memory(ch)) if ch.number == n => Ok(ch.clone()),
                other => Err(EngineError::Mismatch(format!("unexpected memory answer {other:?}"))),
            },
            // Yaesu answers ?; for an empty channel.
            Err(EngineError::Refused(_)) if self.caps.kind == crate::rig::RigKind::YaesuFt991a => {
                Ok(crate::memory::MemoryChannel::empty(n))
            }
            Err(e) => Err(e),
        }
    }

    /// Write one channel and read it back.
    pub fn write_memory(&self, ch: &crate::memory::MemoryChannel) -> EngineResult<crate::memory::MemoryChannel> {
        self.request(Request::WriteMemory(ch.clone()))?;
        let back = self.read_memory(ch.number)?;
        if !back.same_content(ch) {
            return Err(EngineError::Mismatch(format!("channel {} reads back differently", ch.number)));
        }
        Ok(back)
    }

    /// Stop the engine, unkeying first if it keyed the radio. Safe to call twice.
    pub fn shutdown(&self) {
        let _ = self.cmd.lock().unwrap().send(Cmd::Shutdown);
        let t = self.thread.lock().unwrap().take();
        if let Some(t) = t {
            let _ = t.join();
        }
        self.state.lock().unwrap().connected = false;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Worker {
    transport: Box<dyn Transport>,
    protocol: Box<dyn Protocol>,
    config: EngineConfig,
    cmd: Receiver<Cmd>,
    state: Arc<Mutex<RadioState>>,
    subscribers: Arc<Mutex<Vec<Sender<Event>>>>,
    queue: VecDeque<(Request, ReplyTo, Option<Duration>)>,
    outstanding: Option<Outstanding>,
    polls: Vec<PollState>,
    tx_deadline: Option<Instant>,
    we_keyed: bool,
    stats: EngineStats,
}

impl Worker {
    fn new(
        transport: Box<dyn Transport>,
        protocol: Box<dyn Protocol>,
        config: EngineConfig,
        cmd: Receiver<Cmd>,
        state: Arc<Mutex<RadioState>>,
        subscribers: Arc<Mutex<Vec<Sender<Event>>>>,
    ) -> Self {
        let now = Instant::now();
        let polls = config.polls.iter().map(|item| PollState { item: item.clone(), next_due: now, backoff: None }).collect();
        let mut queue = VecDeque::new();
        if config.auto_information && protocol.encode(&Request::AutoInformation(true)).is_some() {
            queue.push_back((Request::AutoInformation(true), None, None));
        }
        Self {
            transport,
            protocol,
            config,
            cmd,
            state,
            subscribers,
            queue,
            outstanding: None,
            polls,
            tx_deadline: None,
            we_keyed: false,
            stats: EngineStats::default(),
        }
    }

    fn run(mut self) {
        let mut buf = [0u8; 4096];
        let mut shutting_down = false;
        let mut shutdown_deadline = None;
        loop {
            // 1. Commands from the app.
            loop {
                match self.cmd.try_recv() {
                    Ok(Cmd::Request { req, reply, tx_limit }) => {
                        if shutting_down {
                            let _ = reply.send(Err(EngineError::Stopped));
                        } else if req.keys_transmitter() {
                            // Transmit goes to the front so it is not delayed by queued reads.
                            self.queue.push_front((req, Some(reply), tx_limit.or(Some(self.config.default_tx_limit))));
                        } else {
                            self.queue.push_back((req, Some(reply), None));
                        }
                    }
                    Ok(Cmd::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                        if !shutting_down {
                            shutting_down = true;
                            shutdown_deadline = Some(Instant::now() + self.config.reply_timeout * 2);
                            for (_, reply, _) in self.queue.drain(..) {
                                if let Some(r) = reply {
                                    let _ = r.send(Err(EngineError::Stopped));
                                }
                            }
                            if self.we_keyed {
                                self.queue.push_back((Request::SetTx(false), None, None));
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if shutting_down && self.outstanding.is_none() && self.queue.is_empty() {
                break;
            }
            if shutdown_deadline.is_some_and(|d| Instant::now() > d) {
                break;
            }

            // 2. Transmit watchdog.
            if let Some(d) = self.tx_deadline
                && Instant::now() >= d
            {
                self.tx_deadline = None;
                self.queue.push_front((Request::SetTx(false), None, None));
            }

            // 3. Send the next command if the line is free.
            if self.outstanding.is_none() {
                let next = self.queue.pop_front().map(|(r, reply, lim)| (r, reply, lim, None)).or_else(|| {
                    if shutting_down {
                        return None;
                    }
                    self.next_poll().map(|i| (self.polls[i].item.request.clone(), None, None, Some(i)))
                });
                if let Some((req, reply, tx_limit, poll_index)) = next
                    && let Err(e) = self.send(req, reply, tx_limit, poll_index)
                {
                    self.link_failed(e);
                    break;
                }
            }

            // 4. Read and dispatch.
            let wait = match &self.outstanding {
                Some(o) => o.deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(10)),
                None => Duration::from_millis(5),
            };
            match self.transport.read(&mut buf, wait.max(Duration::from_millis(1))) {
                Ok(0) => {}
                Ok(n) => {
                    self.stats.bytes_in += n as u64;
                    let msgs = self.protocol.feed(&buf[..n]);
                    for m in msgs {
                        self.dispatch(m);
                    }
                }
                Err(e) => {
                    self.link_failed(e.to_string());
                    break;
                }
            }

            // 5. Timeouts.
            if let Some(o) = &self.outstanding
                && Instant::now() >= o.deadline
            {
                let o = self.outstanding.take().unwrap();
                self.stats.timeouts += 1;
                if let Some(i) = o.poll_index {
                    let p = &mut self.polls[i];
                    p.next_due = Instant::now() + p.item.interval;
                }
                if let Some(r) = o.reply {
                    let _ = r.send(Err(EngineError::Timeout(o.name)));
                }
            }
            self.publish_stats();
        }
        let mut st = self.state.lock().unwrap();
        st.connected = false;
        st.tx = false;
    }

    fn link_failed(&mut self, e: String) {
        if let Some(o) = self.outstanding.take()
            && let Some(r) = o.reply
        {
            let _ = r.send(Err(EngineError::Io(e.clone())));
        }
        for (_, reply, _) in self.queue.drain(..) {
            if let Some(r) = reply {
                let _ = r.send(Err(EngineError::Io(e.clone())));
            }
        }
        let mut st = self.state.lock().unwrap();
        st.link_error = Some(e);
        st.connected = false;
    }

    fn next_poll(&self) -> Option<usize> {
        let now = Instant::now();
        let tx = self.state.lock().unwrap().tx;
        self.polls
            .iter()
            .enumerate()
            .filter(|(_, p)| p.next_due <= now && (!p.item.tx_only || tx))
            .min_by_key(|(_, p)| p.next_due)
            .map(|(i, _)| i)
    }

    fn send(
        &mut self,
        req: Request,
        reply: Option<Sender<EngineResult<Reply>>>,
        tx_limit: Option<Duration>,
        poll_index: Option<usize>,
    ) -> Result<(), String> {
        let name = req.name();
        let Some(enc) = self.protocol.encode(&req) else {
            if let Some(i) = poll_index {
                // This radio cannot do it at all; stop polling it.
                self.polls[i].next_due = Instant::now() + Duration::from_secs(24 * 3600);
            }
            if let Some(r) = reply {
                let _ = r.send(Err(EngineError::Unsupported(name)));
            }
            return Ok(());
        };
        self.transport.write_all(&enc.bytes).map_err(|e| e.to_string())?;
        self.stats.sent += 1;
        self.stats.bytes_out += enc.bytes.len() as u64;
        let keyed_tx = req.keys_transmitter();
        if keyed_tx {
            self.we_keyed = true;
            self.tx_deadline = tx_limit.map(|l| Instant::now() + l);
        }
        if matches!(req, Request::SetTx(false)) {
            self.tx_deadline = None;
            self.we_keyed = false;
        }
        if enc.expect == Expect::Nothing {
            if let Some(i) = poll_index {
                self.polls[i].next_due = Instant::now() + self.polls[i].item.interval;
            }
            if keyed_tx || matches!(req, Request::SetTx(false)) {
                self.apply(Event::Tx(keyed_tx));
            }
            if let Some(r) = reply {
                let _ = r.send(Ok(Reply::Sent));
            }
            return Ok(());
        }
        self.outstanding = Some(Outstanding {
            name,
            expect: enc.expect,
            deadline: Instant::now() + self.config.reply_timeout,
            reply,
            poll_index,
            tx_change: match req {
                Request::SetTx(on) => Some(on),
                _ => None,
            },
        });
        Ok(())
    }

    fn dispatch(&mut self, m: Incoming) {
        self.stats.messages += 1;
        if let Incoming::Message { events, .. } = &m {
            for e in events.clone() {
                self.apply(e);
            }
        }
        let Some(o) = &self.outstanding else { return };
        let result = match (&o.expect, &m) {
            (Expect::Ack, Incoming::Ok) => Some(Ok(Reply::Ack)),
            (_, Incoming::Ng) => Some(Err(EngineError::Refused(o.name))),
            (Expect::Reply(key), Incoming::Message { payload, events }) if payload.starts_with(key) => {
                Some(Ok(Reply::Data(events.clone())))
            }
            _ => None,
        };
        let Some(result) = result else { return };
        let o = self.outstanding.take().unwrap();
        if let Some(i) = o.poll_index {
            let max = self.config.max_backoff;
            let p = &mut self.polls[i];
            match &result {
                Err(EngineError::Refused(_)) => {
                    let next = p.backoff.map_or(Duration::from_secs(1), |b| (b * 2).min(max)).max(p.item.interval);
                    p.backoff = Some(next);
                    p.next_due = Instant::now() + next;
                }
                _ => {
                    p.backoff = None;
                    p.next_due = Instant::now() + p.item.interval;
                }
            }
        }
        if let Err(EngineError::Refused(n)) = &result {
            self.stats.refused += 1;
            *self.stats.refused_by_command.entry(n).or_default() += 1;
        }
        if let (Some(on), true) = (o.tx_change, result.is_ok()) {
            self.apply(Event::Tx(on));
        }
        if let Some(r) = o.reply {
            let _ = r.send(result);
        }
    }

    fn apply(&mut self, e: Event) {
        let mut mode_changed = false;
        {
            let mut st = self.state.lock().unwrap();
            match &e {
                Event::Id(id) => st.id = Some(*id),
                Event::Frequency(f) => st.frequency_hz = Some(*f),
                Event::Mode(m) => {
                    mode_changed = st.mode.map(|o| o.mode) != Some(m.mode);
                    st.mode = Some(*m);
                }
                Event::ModeHint(m) => {
                    let data = st.mode.is_some_and(|o| o.data);
                    mode_changed = st.mode.map(|o| o.mode) != Some(m.mode);
                    st.mode = Some(ModeSetting { data, ..*m });
                }
                Event::SMeter(v) => st.s_meter = Some(*v),
                Event::PowerLevel(v) => st.power_pct = Some(*v),
                Event::Po(v) => st.po = Some(*v),
                Event::Swr(v) => st.swr = Some(*v),
                Event::Alc(v) => st.alc = Some(*v),
                Event::Tx(t) => st.tx = *t,
                Event::ScopeOn(on) => st.scope_on = Some(*on),
                Event::Sweep(_) | Event::Memory(_) | Event::ClockTime { .. } | Event::AutoInformation(_) => {}
            }
        }
        if matches!(e, Event::Tx(false)) {
            self.tx_deadline = None;
            self.we_keyed = false;
        }
        if mode_changed {
            for p in &mut self.polls {
                if p.backoff.take().is_some() {
                    p.next_due = Instant::now();
                }
            }
        }
        if matches!(e, Event::ModeHint(_)) {
            // Pick up the DATA setting the hint does not carry.
            for p in &mut self.polls {
                if p.item.request == Request::ReadMode {
                    p.next_due = Instant::now();
                }
            }
        }
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|s| s.send(e.clone()).is_ok());
    }

    fn publish_stats(&mut self) {
        let sc = self.protocol.scope_stats();
        self.stats.sweeps = sc.sweeps;
        self.stats.sweeps_dropped = sc.dropped;
        self.stats.backed_off = self.polls.iter().filter_map(|p| p.backoff.map(|b| (p.item.request.name(), b))).collect();
        self.state.lock().unwrap().stats = self.stats.clone();
    }
}

/// Convenience: the last sweep from a subscription, discarding older ones.
pub fn latest_sweep(rx: &Receiver<Event>) -> Option<Sweep> {
    let mut last = None;
    while let Ok(e) = rx.try_recv() {
        if let Event::Sweep(s) = e {
            last = Some(s);
        }
    }
    last
}
