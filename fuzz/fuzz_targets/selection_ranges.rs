#![no_main]

use libfuzzer_sys::fuzz_target;
use term_core::{TerminalSize, TerminalState};

mod support;

fuzz_target!(|data: &[u8]| {
    let mut terminal = TerminalState::new(TerminalSize::new(24, 8));
    support::fill_unicode_grid(&mut terminal, data);

    for chunk in data.chunks(8) {
        let a = u16_from(chunk, 0);
        let b = u16_from(chunk, 2);
        let c = u16_from(chunk, 4);
        let d = u16_from(chunk, 6);
        support::set_fuzz_selection(
            &mut terminal,
            a,
            b,
            c,
            d,
            chunk.first().is_some_and(|v| v % 2 == 0),
        );
        support::assert_terminal_invariants(&terminal);
    }
});

fn u16_from(data: &[u8], index: usize) -> u16 {
    u16::from_le_bytes([
        data.get(index).copied().unwrap_or_default(),
        data.get(index + 1).copied().unwrap_or_default(),
    ])
}
