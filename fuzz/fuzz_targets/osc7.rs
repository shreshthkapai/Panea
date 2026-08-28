#![no_main]

use libfuzzer_sys::fuzz_target;
use semantics::BufferPosition;
use shell_integration::SemanticEscapeParser;
use term_core::{TerminalCore, TerminalSize};
use term_parser::TerminalEmulator;

mod support;

const MAX_COMPONENT_BYTES: usize = 2048;

fuzz_target!(|data: &[u8]| {
    let split = data.len() / 3;
    let host = &data[..split.min(MAX_COMPONENT_BYTES)];
    let path_end = split
        .saturating_add(MAX_COMPONENT_BYTES)
        .min(data.len());
    let path = &data[split..path_end];

    let mut sequence = Vec::with_capacity(host.len() + path.len() + 12);
    sequence.extend_from_slice(b"\x1b]7;file://");
    sequence.extend_from_slice(host);
    sequence.push(b'/');
    sequence.extend_from_slice(path);
    if data.last().is_some_and(|byte| byte & 1 == 0) {
        sequence.push(0x07);
    } else {
        sequence.extend_from_slice(b"\x1b\\");
    }

    let mut semantic = SemanticEscapeParser::new();
    let _ = semantic.parse(&sequence, BufferPosition::new(0, 0));

    let mut terminal = TerminalEmulator::new(TerminalSize::new(24, 8));
    for chunk in sequence.chunks(31) {
        terminal.apply_bytes(chunk).unwrap();
        support::assert_terminal_invariants(terminal.state());
        support::assert_pending_output_invariants(terminal.state_mut());
    }
});
