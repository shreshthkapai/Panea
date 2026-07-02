#![no_main]

use libfuzzer_sys::fuzz_target;
use term_core::{TerminalCore, TerminalSize};
use term_parser::TerminalEmulator;

mod support;

fuzz_target!(|data: &[u8]| {
    let mut terminal = TerminalEmulator::new(TerminalSize::new(24, 8));

    terminal.apply_bytes(data).unwrap();
    support::assert_terminal_invariants(terminal.state());

    let mut osc_bel = Vec::with_capacity(data.len() + 3);
    osc_bel.extend_from_slice(b"\x1b]");
    osc_bel.extend_from_slice(data);
    osc_bel.push(0x07);
    terminal.apply_bytes(&osc_bel).unwrap();
    support::assert_terminal_invariants(terminal.state());

    let mut osc_st = Vec::with_capacity(data.len() + 4);
    osc_st.extend_from_slice(b"\x1b]");
    osc_st.extend_from_slice(data);
    osc_st.extend_from_slice(b"\x1b\\");
    terminal.apply_bytes(&osc_st).unwrap();
    support::assert_terminal_invariants(terminal.state());

    let mut dcs_like = Vec::with_capacity(data.len() + 4);
    dcs_like.extend_from_slice(b"\x1bP");
    dcs_like.extend_from_slice(data);
    dcs_like.extend_from_slice(b"\x1b\\");
    terminal.apply_bytes(&dcs_like).unwrap();
    support::assert_terminal_invariants(terminal.state());
});
