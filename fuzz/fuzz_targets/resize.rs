#![no_main]

use libfuzzer_sys::fuzz_target;
use term_core::{TerminalCore, TerminalSize, TerminalState};

mod support;

fuzz_target!(|data: &[u8]| {
    let mut terminal = TerminalState::new(TerminalSize::new(12, 4));
    support::fill_unicode_grid(&mut terminal, data);
    support::assert_terminal_invariants(&terminal);

    for chunk in data.chunks(4) {
        let cols = u16::from(chunk.first().copied().unwrap_or(1)).max(1);
        let rows = u16::from(chunk.get(1).copied().unwrap_or(1)).max(1);
        terminal
            .resize(TerminalSize::new((cols % 120).max(1), (rows % 40).max(1)))
            .unwrap();
        if chunk.get(2).is_some_and(|value| value % 2 == 0) {
            support::set_fuzz_selection(
                &mut terminal,
                u16::from(chunk.first().copied().unwrap_or_default()),
                u16::from(chunk.get(1).copied().unwrap_or_default()),
                u16::from(chunk.get(2).copied().unwrap_or_default()),
                u16::from(chunk.get(3).copied().unwrap_or_default()),
                false,
            );
        }
        support::assert_terminal_invariants(&terminal);
    }
});
