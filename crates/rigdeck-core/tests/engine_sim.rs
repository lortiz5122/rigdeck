//! The engine against the simulated radios, over an in-memory link and over a real pty.

use rigdeck_core::engine::{Engine, EngineConfig, EngineError, Reply};
use rigdeck_core::memory::{self, MemoryChannel};
use rigdeck_core::rig::{Event, Mode, ModeSetting, Request, RigKind};
use rigdeck_core::sim::{Ft991aSim, Ic7300Sim, SimHandle};
use rigdeck_core::transport::{PtyMaster, SerialTransport, pipe_pair};
use rigdeck_core::{Protocol, driver};
use std::time::{Duration, Instant};

fn icom(sim: Ic7300Sim, polling: bool) -> (Engine, SimHandle<Ic7300Sim>) {
    let (a, b) = pipe_pair();
    let h = SimHandle::spawn(sim, Box::new(b));
    let p = driver(RigKind::IcomIc7300);
    let mut cfg = EngineConfig::default_for(&p.capabilities());
    if !polling {
        cfg = cfg.without_polling();
    }
    (Engine::start(Box::new(a), p, cfg), h)
}

fn yaesu(sim: Ft991aSim, polling: bool) -> (Engine, SimHandle<Ft991aSim>) {
    let (a, b) = pipe_pair();
    let h = SimHandle::spawn(sim, Box::new(b));
    let p = driver(RigKind::YaesuFt991a);
    let mut cfg = EngineConfig::default_for(&p.capabilities());
    if !polling {
        cfg = cfg.without_polling();
    }
    (Engine::start(Box::new(a), p, cfg), h)
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn icom_read_and_set() {
    let (e, sim) = icom(Ic7300Sim::default(), false);
    assert_eq!(e.read_id().unwrap(), 0x94);
    assert_eq!(e.read_frequency().unwrap(), 14_074_000);
    assert_eq!(e.set_frequency(7_074_000).unwrap(), 7_074_000);
    assert_eq!(sim.radio.lock().unwrap().frequency_hz, 7_074_000);
    let m = e.set_mode(ModeSetting { mode: Mode::Lsb, filter: Some(2), data: false }).unwrap();
    assert_eq!(m.mode, Mode::Lsb);
    assert_eq!(e.set_power(50).unwrap(), 50);
    assert!(e.read_s_meter().unwrap() <= 241);
    assert_eq!(e.set_frequency(80_000_000), Err(EngineError::Unsupported("frequency outside the radio's range")));
}

#[test]
fn yaesu_read_and_set() {
    let (e, sim) = yaesu(Ft991aSim::default(), false);
    assert_eq!(e.read_id().unwrap(), 670);
    assert_eq!(e.read_frequency().unwrap(), 7_074_000);
    assert_eq!(e.set_frequency(146_520_000).unwrap(), 146_520_000);
    let m = e.set_mode(ModeSetting { mode: Mode::Fm, filter: None, data: false }).unwrap();
    assert_eq!(m.mode, Mode::Fm);
    assert_eq!(e.set_power(25).unwrap(), 25);
    assert!(sim.radio.lock().unwrap().auto_information, "engine turns on Auto Information");
    // The FT-991A has no scope over CAT.
    assert_eq!(e.request(Request::ScopeOn(true)), Err(EngineError::Unsupported("scope_on")));
}

#[test]
fn transceive_and_auto_information_update_state() {
    let (e, sim) = icom(Ic7300Sim::default(), false);
    sim.radio.lock().unwrap().turn_dial(21_074_000);
    wait_until("icom dial", 2, || e.state().frequency_hz == Some(21_074_000));
    sim.radio.lock().unwrap().press_mode(Mode::Cw);
    wait_until("icom mode", 2, || e.state().mode.map(|m| m.mode) == Some(Mode::Cw));

    let (y, ysim) = yaesu(Ft991aSim::default(), false);
    wait_until("AI on", 2, || ysim.radio.lock().unwrap().auto_information);
    ysim.radio.lock().unwrap().turn_dial(3_573_000);
    wait_until("yaesu dial", 2, || y.state().frequency_hz == Some(3_573_000));
}

#[test]
fn refused_poll_backs_off_and_resets_on_mode_change() {
    // A radio that refuses the S-meter read: the poll must back off, not repeat every 200 ms.
    let sim = Ic7300Sim::default();
    let (a, b) = pipe_pair();
    struct Refuser(Ic7300Sim);
    impl rigdeck_core::sim::SimRadio for Refuser {
        fn receive(&mut self, bytes: &[u8]) -> Vec<u8> {
            if bytes.windows(2).any(|w| w == [0x15, 0x02]) {
                self.0.refused += 1;
                return vec![0xFE, 0xFE, 0xE0, 0x94, 0xFA, 0xFD];
            }
            self.0.receive(bytes)
        }
        fn tick(&mut self, now: Instant) -> Vec<u8> {
            self.0.tick(now)
        }
    }
    let h = SimHandle::spawn(Refuser(sim), Box::new(b));
    let p = driver(RigKind::IcomIc7300);
    let cfg = EngineConfig::default_for(&p.capabilities());
    let e = Engine::start(Box::new(a), p, cfg);
    std::thread::sleep(Duration::from_millis(3500));
    let refused = h.radio.lock().unwrap().0.refused;
    // Without backoff: ~17 refusals in 3.5 s at 200 ms. With 1 s, 2 s backoff: 2–3.
    assert!(refused <= 4, "S-meter asked {refused} times");
    let st = e.state();
    assert!(st.stats.backed_off.iter().any(|(n, _)| *n == "read_s_meter"));
    assert!(st.stats.refused_by_command.get("read_s_meter").copied().unwrap_or(0) >= 1);
    // Other polls kept working.
    assert_eq!(st.frequency_hz, Some(14_074_000));
}

#[test]
fn transmit_watchdog_unkeys() {
    let (e, sim) = icom(Ic7300Sim::default(), false);
    assert_eq!(e.request(Request::SetTx(true)), Err(EngineError::Unsupported("use transmit() to key the radio")));
    assert_eq!(e.transmit(true, Duration::from_millis(300)).unwrap(), Reply::Ack);
    assert!(sim.radio.lock().unwrap().tx);
    assert!(e.state().tx);
    wait_until("watchdog", 2, || !sim.radio.lock().unwrap().tx);
    wait_until("state", 1, || !e.state().tx);
}

#[test]
fn disconnect_while_transmitting_unkeys_first() {
    for kind in RigKind::ALL {
        let (e, unkeyed): (Engine, Box<dyn Fn() -> bool>) = match kind {
            RigKind::IcomIc7300 => {
                let (e, sim) = icom(Ic7300Sim::default(), true);
                (e, Box::new(move || !sim.radio.lock().unwrap().tx))
            }
            RigKind::YaesuFt991a => {
                let (e, sim) = yaesu(Ft991aSim::default(), true);
                (e, Box::new(move || !sim.radio.lock().unwrap().tx))
            }
        };
        e.transmit(true, Duration::from_secs(60)).unwrap();
        assert!(!unkeyed());
        e.shutdown();
        assert!(unkeyed(), "{kind:?} still transmitting after disconnect");
        assert!(!e.is_running());
        assert_eq!(e.read_frequency(), Err(EngineError::Stopped));
    }
}

#[test]
fn icom_memories_write_read_verify() {
    let (e, sim) = icom(Ic7300Sim::default(), false);
    let planned = rigdeck_core::sim::sample_memories(RigKind::IcomIc7300);
    for ch in &planned {
        e.write_memory(ch).unwrap();
    }
    let back: Vec<MemoryChannel> = (1..=5).map(|n| e.read_memory(n).unwrap()).collect();
    assert!(memory::verify(&planned, &back).is_empty());
    assert!(e.read_memory(6).unwrap().empty);
    assert!(sim.radio.lock().unwrap().memory(4).unwrap().mode.data);
    // Clearing a channel.
    e.write_memory(&MemoryChannel::empty(5)).unwrap();
    assert!(e.read_memory(5).unwrap().empty);
    // P1 and P2.
    e.write_memory(&MemoryChannel::new(100, 14_000_000, Mode::Cw, "P1 edge")).unwrap();
    assert_eq!(e.read_memory(100).unwrap().name, "P1 edge");
}

#[test]
fn yaesu_memories_write_read_verify() {
    let (e, _sim) = yaesu(Ft991aSim::default(), false);
    let planned = rigdeck_core::sim::sample_memories(RigKind::YaesuFt991a);
    for ch in &planned {
        e.write_memory(ch).unwrap();
    }
    let back: Vec<MemoryChannel> = (1..=6).map(|n| e.read_memory(n).unwrap()).collect();
    assert!(memory::verify(&planned, &back).is_empty(), "{:?}", memory::verify(&planned, &back));
    assert!(e.read_memory(50).unwrap().empty);
}

#[test]
fn icom_scope_through_engine() {
    let mut sim = Ic7300Sim::default();
    sim.sweeps_per_second = 50.0;
    let (e, h) = icom(sim, true);
    let rx = e.subscribe();
    e.request(Request::ScopeOn(true)).unwrap();
    e.request(Request::ScopeDataOutput(true)).unwrap();
    let mut sweeps = 0;
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        if let Ok(Event::Sweep(s)) = rx.recv_timeout(Duration::from_millis(100)) {
            assert_eq!(s.pixels.len(), 475);
            assert_eq!(s.low_hz, 14_074_000 - 25_000);
            sweeps += 1;
        }
    }
    assert!(sweeps >= 60, "only {sweeps} sweeps in 2 s at 50/s");
    let st = e.state();
    assert_eq!(st.stats.sweeps_dropped, 0);
    // Polling kept running while the scope streamed.
    assert!(st.s_meter.is_some());
    e.request(Request::ScopeDataOutput(false)).unwrap();
    assert!(!h.radio.lock().unwrap().scope_output);
}

#[test]
fn icom_scope_with_425_pixels() {
    let mut sim = Ic7300Sim::default();
    sim.pixels = 425;
    sim.scope_output = true;
    let (e, _h) = icom(sim, false);
    let rx = e.subscribe();
    let s = loop {
        if let Ok(Event::Sweep(s)) = rx.recv_timeout(Duration::from_secs(2)) {
            break s;
        }
    };
    assert_eq!(s.pixels.len(), 425);
}

#[test]
fn over_a_real_pty_like_a_serial_port() {
    for kind in RigKind::ALL {
        let master = PtyMaster::open().unwrap();
        let path = master.slave_path.clone();
        let _sim: Box<dyn std::any::Any> = match kind {
            RigKind::IcomIc7300 => Box::new(SimHandle::spawn(Ic7300Sim::default(), Box::new(master))),
            RigKind::YaesuFt991a => Box::new(SimHandle::spawn(Ft991aSim::default(), Box::new(master))),
        };
        let t = SerialTransport::open(&path, kind.default_baud()).unwrap();
        // A second open of the same port must fail: TIOCEXCL.
        assert!(SerialTransport::open(&path, kind.default_baud()).is_err(), "port not exclusive");
        let p = driver(kind);
        let cfg = EngineConfig::default_for(&p.capabilities());
        let e = Engine::start(Box::new(t), p, cfg);
        assert!(e.read_id().is_ok());
        assert!(e.read_frequency().is_ok());
        e.shutdown();
        // After disconnect the port is free again.
        drop(SerialTransport::open(&path, kind.default_baud()).unwrap());
    }
}

#[test]
fn link_loss_is_reported_not_hung() {
    let (a, b) = pipe_pair();
    let p = driver(RigKind::IcomIc7300);
    let e = Engine::start(Box::new(a), p, EngineConfig::default_for(&RigKind::IcomIc7300.capabilities()).without_polling());
    drop(b); // unplugged
    wait_until("link error", 2, || !e.is_running());
    assert!(e.state().link_error.is_some());
    assert!(matches!(e.read_frequency(), Err(EngineError::Stopped) | Err(EngineError::Io(_))));
}

#[test]
fn no_answer_times_out() {
    let (a, _b) = pipe_pair(); // nobody on the other end, but the link stays up
    let p = driver(RigKind::YaesuFt991a);
    let e = Engine::start(Box::new(a), p, EngineConfig::default_for(&RigKind::YaesuFt991a.capabilities()).without_polling());
    assert_eq!(e.read_frequency(), Err(EngineError::Timeout("read_frequency")));
    assert_eq!(e.state().stats.timeouts, 1);
}

#[test]
fn csv_round_trip_through_the_radio() {
    let (e, _sim) = icom(Ic7300Sim::default(), false);
    let csv = "Memory,Rank,Channel,Freq_MHz,Mode_Written,Primary_Modes_As_Pasted,Name,Usage\n\
               1,1,20,14.074,USB,USB,FT8 20m,x\n2,2,40,7.074,USB,USB,FT8 40m,y\n3,3,40,7.200,LSB,LSB,40m SSB,z\n";
    let planned = memory::import_csv(RigKind::IcomIc7300, csv).unwrap();
    let before: Vec<_> = (1..=3).map(|n| e.read_memory(n).unwrap()).collect();
    let changes = memory::preview(&before, &planned);
    assert!(changes.iter().all(|c| c.kind == memory::ChangeKind::New));
    for ch in &planned {
        e.write_memory(ch).unwrap();
    }
    let after: Vec<_> = (1..=3).map(|n| e.read_memory(n).unwrap()).collect();
    assert!(memory::verify(&planned, &after).is_empty());
    let exported = memory::export_csv(RigKind::IcomIc7300, &after);
    let again = memory::import_csv(RigKind::IcomIc7300, &exported).unwrap();
    assert_eq!(again, after);
}

#[test]
fn protocol_trait_reports_kind() {
    for k in RigKind::ALL {
        let p: Box<dyn Protocol> = driver(k);
        assert_eq!(p.kind(), k);
    }
}
