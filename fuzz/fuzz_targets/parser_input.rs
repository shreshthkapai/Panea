#![no_main]

use libfuzzer_sys::fuzz_target;
use term_core::{TerminalCore, TerminalSize};
use term_parser::TerminalEmulator;

mod support;

fuzz_target!(|data: &[u8]| {
    let mut terminal = TerminalEmulator::new(TerminalSize::new(24, 8));
    for chunk in data.chunks(31) {
        terminal.apply_bytes(chunk).unwrap();
        support::assert_terminal_invariants(terminal.state());
        support::assert_pending_output_invariants(terminal.state_mut());
    }
    support::assert_alt_screen_saved_cursor_round_trip(data);
});
