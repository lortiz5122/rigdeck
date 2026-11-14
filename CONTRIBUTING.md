# Contributing to RigDeck

Thanks for helping. Bug reports, test results from real radios, new radio drivers and code changes are all welcome.

## Reporting a bug or a test result

Open an issue and include:

- the radio model and its firmware version;
- your macOS version and Mac model;
- what you did, what you expected, and what happened;
- for connection problems, the output of `rigdeck-tools ports` and `rigdeck-tools probe --rig <ic7300|ft991a>`.

FT-991A owners: the FT-991A driver has not been tried on a real radio yet. A report of what works and what does not is very useful.

Please do not attach memory backups or other files that contain your own channels unless you mean to share them.

## Making a change

1. Fork the repository and create a branch from `main`.
2. Make the change. Keep it focused on one thing.
3. Run the checks:

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets
   cargo test --workspace
   ```

4. If you changed the UI, run the app (`scripts/bundle.sh`, then open `dist/RigDeck.app`, or use `--simulator ic7300`) and check the change by eye. Add a screenshot to the pull request.
5. Open a pull request that says what changed, why, and how you tested it (simulator only, or which real radio).

## Guidelines

- **Protocol code comes from the manufacturers' documentation.** When you add or change a radio command, add it to the radio's catalogue in `crates/rigdeck-core/catalogue/` with the manual page it comes from. Do not copy code or rig files from GPL-licensed projects; RigDeck is MIT licensed.
- **Every protocol change has a test.** `rigdeck-core` has no UI and runs against the built-in simulators, so most behaviour can be tested with `cargo test`. Extend the simulator in `sim.rs` when a new command needs it.
- **Transmit stays safe.** Anything that can key the radio needs an explicit user action and must unkey on disconnect or time-out. Network servers stay on loopback and with transmit off by default.
- **The UI never blocks on the radio.** Radio work happens on the engine thread or a background thread.

## Adding a radio

Implement the `Protocol` trait (see `civ.rs` and `cat.rs`), add the radio to `RigKind` in `rig.rs` with its capabilities, add a simulator in `sim.rs`, and add tests. Open an issue first if you want to discuss the approach.

## Licence

By contributing, you agree that your contribution is licensed under the MIT licence in [LICENSE](LICENSE).
