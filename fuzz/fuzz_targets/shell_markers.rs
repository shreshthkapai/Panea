#![no_main]

use libfuzzer_sys::fuzz_target;
use semantics::BufferPosition;
use shell_integration::{SemanticEscapeParser, parse_osc_payload};

fuzz_target!(|data: &[u8]| {
    let position = BufferPosition::new(0, 0);
    let mut parser = SemanticEscapeParser::new();

    let _ = parser.parse(data, position);
    let _ = parse_osc_payload(data, position);

    let mut wrapped = Vec::with_capacity(data.len() + 3);
    wrapped.extend_from_slice(b"\x1b]");
    wrapped.extend_from_slice(data);
    wrapped.push(0x07);
    let _ = parser.parse(&wrapped, position);
});
