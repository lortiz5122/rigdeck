//! RigDeck: a native macOS control app for the Icom IC-7300 and Yaesu FT-991A.
//!
//! Options (for automated checks):
//!   --simulator ic7300|ft991a   connect to the built-in simulator at launch
//!   --port /dev/cu.xxx          connect to this serial port at launch (radio from saved settings)
//!   --screenshot FILE --after S save a PNG of the window after S seconds
//!   --open-memories             open the Memories window
//!   --read-memories             read all channels after connecting
//!   --disconnect-after S        disconnect after S seconds (windows stay open)
//!   --cycles N                  with --disconnect-after: reconnect and disconnect N times
//!   --tune-memory N             tune to memory channel N once memories are read (as a click does)
//!   --theme <name> (any theme name, e.g. nord, dracula, night), --colours classic|smartsdr|turbo|viridis|inferno|gray,
//!   --brightness X (0.25 darker … 4 lighter)
//!   --exit-after S              quit after S seconds

mod app;
mod donate;
mod memories;
mod theme;
mod waterfall;

use app::{LaunchOptions, RigDeckApp};
use eframe::egui;
use rigdeck_core::RigKind;

fn parse_args() -> LaunchOptions {
    let mut o = LaunchOptions::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let num = |s: Option<&String>| s.and_then(|v| v.parse::<f32>().ok());
    while i < args.len() {
        match args[i].as_str() {
            "--simulator" => {
                o.simulator = match args.get(i + 1).map(String::as_str) {
                    Some("ft991a") => Some(RigKind::YaesuFt991a),
                    _ => Some(RigKind::IcomIc7300),
                };
                i += 1;
            }
            "--port" => {
                o.port = args.get(i + 1).cloned();
                i += 1;
            }
            "--screenshot" => {
                o.screenshot = args.get(i + 1).map(Into::into);
                i += 1;
            }
            "--after" => {
                o.screenshot_after = num(args.get(i + 1)).unwrap_or(2.0);
                i += 1;
            }
            "--open-memories" => o.open_memories = true,
            "--read-memories" => o.read_memories = true,
            "--disconnect-after" => {
                o.disconnect_after = num(args.get(i + 1));
                i += 1;
            }
            "--cycles" => {
                o.cycles = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0);
                i += 1;
            }
            "--scroll-notches" => {
                o.scroll_notches = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0);
                i += 1;
            }
            "--click-at" => {
                o.click_at = num(args.get(i + 1));
                i += 1;
            }
            "--tune-memory" => {
                o.tune_memory = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 1;
            }
            "--theme" => {
                let want = args.get(i + 1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
                o.theme = theme::AppTheme::ALL
                    .into_iter()
                    .find(|t| t.name().to_ascii_lowercase().replace(' ', "").starts_with(&want.replace(['-', ' '], "")));
                i += 1;
            }
            "--colours" => {
                o.colour_map = match args.get(i + 1).map(String::as_str) {
                    Some("smartsdr") => Some(waterfall::ColourMap::SmartSdr),
                    Some("turbo") => Some(waterfall::ColourMap::Turbo),
                    Some("viridis") => Some(waterfall::ColourMap::Viridis),
                    Some("inferno") => Some(waterfall::ColourMap::Inferno),
                    Some("gray") => Some(waterfall::ColourMap::Grayscale),
                    Some("red") => Some(waterfall::ColourMap::NightRed),
                    Some("ocean") => Some(waterfall::ColourMap::Ocean),
                    _ => Some(waterfall::ColourMap::Classic),
                };
                i += 1;
            }
            "--brightness" => {
                o.gamma = num(args.get(i + 1)).map(|b| 1.0 / b.clamp(0.25, 4.0));
                i += 1;
            }
            "--exit-after" => {
                o.exit_after = num(args.get(i + 1));
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    if o.screenshot.is_some() && o.screenshot_after == 0.0 {
        o.screenshot_after = 2.0;
    }
    o
}

fn main() -> eframe::Result {
    let opts = parse_args();
    // RIGDECK_WINDOW=WxH fixes the window size (and does not save it), for repeatable CPU checks.
    let fixed: Option<[f32; 2]> = std::env::var("RIGDECK_WINDOW").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some([w.parse().ok()?, h.parse().ok()?])
    });
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RigDeck")
            .with_inner_size(fixed.unwrap_or([1100.0, 760.0]))
            .with_min_inner_size([760.0, 520.0]),
        renderer: eframe::Renderer::Wgpu,
        // Checks set RIGDECK_FIXED_WINDOW so CPU measurements use the same window size every time.
        persist_window: fixed.is_none() && std::env::var_os("RIGDECK_FIXED_WINDOW").is_none(),
        ..Default::default()
    };
    eframe::run_native("RigDeck", native, Box::new(move |cc| Ok(Box::new(RigDeckApp::new(cc, opts)))))
}
