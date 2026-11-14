# RigDeck

**The closest you can get to an SDR on your Icom 7300 and Yaesu FT-991A.**

RigDeck is a native macOS control app for the **Icom IC-7300** (CI-V over USB) and the **Yaesu FT-991A** (CAT over USB). It gives your radio a live spectrum scope and GPU waterfall you can click to tune, a full control panel, a memory editor, and a rigctld server for WSJT-X and loggers. It is written in Rust from the manufacturers' published command documentation and is MIT licensed.

## What it does

| Feature | IC-7300 | FT-991A |
|---|---|---|
| Spectrum scope and waterfall: click a signal to tune, drag or scroll to tune, span, centre/fixed, speed, passband overlay | yes | no (the FT-991A does not send its scope over CAT) |
| Waterfall colour maps (classic, deep blue/green, turbo, viridis, inferno, grayscale) with floor, ceiling and brightness | yes | — |
| Frequency: digit scroll, click, keyboard entry, band buttons, tuning step, arrow keys | yes | yes |
| Mode, DATA, filter | yes | yes (no numbered filters over CAT) |
| Meters: S, and Po / SWR / ALC while transmitting | yes | yes |
| RF power | yes | yes (5–100 W) |
| Transmit with a confirmation and an automatic unkey timer | yes | yes |
| Follows the radio's own dial and buttons (CI-V transceive / Auto Information) | yes | yes |
| Memory list in the sidebar: click a channel to tune to it | yes | yes |
| Memory editor: read all, edit, CSV import with a preview, backup before every write, write with read-back verify | yes (1–99, P1, P2) | yes (1–117) |
| rigctld server for WSJT-X and loggers (loopback by default, LAN allow-list, transmit off by default) | yes | yes |
| TCI server (WebSocket, loopback, transmit off by default) | yes | yes |
| Set the radio clock from the Mac (UTC) | yes | no CAT command |
| Reconnects by itself after a USB unplug or radio power-off | yes | yes |
| Built-in simulator, so you can try it without a radio | yes | yes |
| 16 themes, including a red night-vision theme, and an interface size setting | yes | yes |

Test status: the IC-7300 driver has been run against a real radio. The FT-991A driver is written from Yaesu's CAT manual and passes the simulator tests, but has **not yet been tried on a real FT-991A**. Reports from FT-991A owners are welcome (see [Contributing](#contributing)).

## Support development

RigDeck is free and open source, with no ads, no account and no subscription. It is built by a ham for the ham radio community. Donations pay for further development of the app: support for more radios, new features, fixes, and the time spent testing on real radios. The code stays free for every amateur. Any amount helps. Thank you, and 73!

- **PayPal:** [pay $10 with PayPal](https://www.paypal.com/ncp/payment/PLB-6CEULH5Q3URL), or send any amount to ortizl@gmail.com
- **Cash App:** [$lortiz1721](https://cash.app/$lortiz1721)

The same buttons are in the app, in the sidebar under **Support RigDeck**.

## Requirements

- macOS 13 or later. Developed and tested on Apple silicon.
- Rust stable, 1.95 or later (the oldest version the dependencies accept). Install it with [rustup](https://rustup.rs).
- A USB cable from the Mac to the radio. Both radios use a Silicon Labs CP210x USB serial bridge. macOS has a built-in driver for it; if your radio's port does not appear, install the CP210x VCP driver from Silicon Labs.

## Build and install

```bash
git clone https://github.com/lortiz5122/rigdeck.git
cd rigdeck
cargo test                  # unit and simulator tests
scripts/bundle.sh           # builds dist/RigDeck.app, signed ad hoc for your Mac
cp -R dist/RigDeck.app /Applications/
```

The app is one native binary that uses only macOS frameworks. It needs nothing else at run time.

To run without bundling: `cargo run --release -p rigdeck-app`.

## Use

1. Set up the radio (below) and connect it to the Mac with USB.
2. Open RigDeck, pick the radio and its serial port, and click Connect.
3. Tune with the frequency digits, the band buttons, the arrow keys, or (IC-7300) by clicking a signal on the waterfall.

Try it without a radio:

```bash
open dist/RigDeck.app --args --simulator ic7300    # or ft991a
```

### Radio settings

- **IC-7300**: CI-V address 94h (the default). For the scope over USB, set CI-V USB Port to "Unlink from [REMOTE]" and CI-V USB Baud Rate to 115200 (IC-7300 Full Manual, section 19). Set CI-V Transceive ON so the app follows the radio's dial.
- **FT-991A**: set menu 031 CAT RATE to match the app (38400 is the fastest). CAT runs on the "Enhanced" port of the radio's dual USB serial bridge.

### WSJT-X

In RigDeck Settings, turn on the rigctld server. In WSJT-X set Rig to "Hamlib NET rigctl", Network Server to `127.0.0.1:4533` and PTT method to CAT. Turn on "Allow clients to transmit" in RigDeck only if WSJT-X should key the radio.

### Memories

Memories are read from and written to the radio. Before any write, RigDeck saves a CSV backup of every channel in `~/RigDeck/backups`, then writes each channel and reads it back to check it. CSV import accepts RigDeck's own export and Icom CS-7300 exports.

### Command-line tools

`rigdeck-tools` is built alongside the app (and copied into `RigDeck.app/Contents/MacOS/`):

```bash
rigdeck-tools ports                         # serial ports and the radios recognised on them
rigdeck-tools probe --rig ic7300            # ID, frequency, mode, S-meter
rigdeck-tools control-check --rig ic7300    # set and read back every band, mode, filter and power level; restores the radio
rigdeck-tools memories-read --rig ic7300 --range 1-99 --out memories.csv
rigdeck-tools rigctld --rig ic7300          # headless rigctld on 127.0.0.1:4533
rigdeck-tools sim ic7300                    # simulator on a pseudo-terminal; prints its /dev/ttys path
```

`--rig` is `ic7300` or `ft991a`. Without `--port`, the radio is found by its USB description. The full list of commands is at the top of `crates/rigdeck-tools/src/main.rs`.

## How it is built

```
crates/rigdeck-core     no UI: drivers, engine, simulators, memories, scope, rigctld, TCI
  catalogue/*.json      command catalogues, each entry with its manual page
crates/rigdeck-app      the macOS app (egui on wgpu, which runs on Metal)
crates/rigdeck-tools    command-line tools
scripts/bundle.sh       builds dist/RigDeck.app
```

Each radio has a driver behind one `Protocol` trait (`civ.rs` for Icom, `cat.rs` for Yaesu). One engine thread owns the serial link: it sends one command at a time, matches replies, polls in the background, backs off commands the radio refuses, and unkeys the radio when a transmit time limit runs out or the app disconnects. The UI never waits on the radio.

## Contributing

Contributions are welcome: bug reports, FT-991A test results, new radios, and code. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

MIT. See [LICENSE](LICENSE). RigDeck is not affiliated with Icom or Yaesu. Icom and Yaesu manuals are not included in this repository.
