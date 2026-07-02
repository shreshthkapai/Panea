#![no_main]

use libfuzzer_sys::fuzz_target;
use term_core::{TerminalAction, TerminalSize, TerminalState};

mod support;

fuzz_target!(|data: &[u8]| {
    let mut terminal = TerminalState::new(TerminalSize::new(20, 6));

    for byte in data {
        terminal
            .apply_action(TerminalAction::Print(support::fuzz_char(*byte)))
            .unwrap();
        support::assert_terminal_invariants(&terminal);
    }
});
