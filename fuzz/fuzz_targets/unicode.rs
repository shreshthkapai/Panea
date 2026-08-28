#![no_main]

use libfuzzer_sys::fuzz_target;
use term_core::{TerminalCore, TerminalSize};
use term_parser::TerminalEmulator;

mod support;

fuzz_target!(|data: &[u8]| {
    let mut terminal = TerminalEmulator::new(TerminalSize::new(20, 6));

    for bytes in data.chunks(29) {
        terminal.apply_bytes(bytes).unwrap();
        support::assert_terminal_invariants(terminal.state());
        support::assert_pending_output_invariants(terminal.state_mut());
    }

    for scalar in data.chunks(4) {
        let value = u32::from_le_bytes([
            scalar.first().copied().unwrap_or_default(),
            scalar.get(1).copied().unwrap_or_default(),
            scalar.get(2).copied().unwrap_or_default(),
            scalar.get(3).copied().unwrap_or_default(),
        ]) % 0x11_0000;
        let ch = char::from_u32(value).unwrap_or('\u{fffd}');
        let mut encoded = [0_u8; 4];
        terminal
            .apply_bytes(ch.encode_utf8(&mut encoded).as_bytes())
            .unwrap();
        support::assert_terminal_invariants(terminal.state());
    }
});
