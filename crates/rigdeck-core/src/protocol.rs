//! The interface every radio driver implements. The engine, servers and UI only use this.

use crate::rig::{Capabilities, Event, Request, RigKind};

/// What a sent request waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Nothing comes back (Yaesu set commands).
    Nothing,
    /// An OK/NG acknowledgement (Icom set commands).
    Ack,
    /// A reply whose payload starts with these bytes.
    Reply(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub expect: Expect,
}

/// One message decoded from the radio.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The radio accepted the last command (Icom FB).
    Ok,
    /// The radio refused the last command (Icom FA, Yaesu `?;`).
    Ng,
    /// A data message. `payload` is matched against [`Expect::Reply`]; `events` update state.
    Message { payload: Vec<u8>, events: Vec<Event> },
}

pub trait Protocol: Send {
    fn kind(&self) -> RigKind;
    fn capabilities(&self) -> Capabilities {
        self.kind().capabilities()
    }
    /// Encode a request, or `None` if this radio cannot do it.
    fn encode(&self, req: &Request) -> Option<Encoded>;
    /// Feed received bytes; returns every complete message.
    fn feed(&mut self, bytes: &[u8]) -> Vec<Incoming>;
    /// Scope counters, for radios with a scope.
    fn scope_stats(&self) -> crate::scope::ScopeStats {
        Default::default()
    }
}

/// Make the driver for a radio.
pub fn driver(kind: RigKind) -> Box<dyn Protocol> {
    match kind {
        RigKind::IcomIc7300 => Box::new(crate::civ::IcomCiv::ic7300()),
        RigKind::YaesuFt991a => Box::new(crate::cat::YaesuCat::ft991a()),
    }
}
