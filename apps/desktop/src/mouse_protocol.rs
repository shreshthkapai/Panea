// Terminal mouse protocol state and wire encoding.

#[derive(Debug, Default)]
struct MouseProtocolState {
    pressed_button: Option<MouseButton>,
}

impl MouseProtocolState {
    fn report_bytes(
        &mut self,
        event: MouseEvent,
        metrics: CellMetrics,
        modes: &BTreeSet<TerminalMode>,
    ) -> Option<Vec<u8>> {
        let enabled = modes.contains(&TerminalMode::MouseReporting)
            || modes.contains(&TerminalMode::MouseCellMotion)
            || modes.contains(&TerminalMode::MouseAllMotion);
        if !enabled {
            return None;
        }

        let col = ((event.x / f64::from(metrics.cell_width)).floor() as u16).saturating_add(1);
        let row = ((event.y / f64::from(metrics.cell_height)).floor() as u16).saturating_add(1);

        let report = match event.kind {
            MouseEventKind::Pressed(button) => {
                self.pressed_button = Some(button);
                MouseReport {
                    button_code: mouse_button_code(button)?,
                    pressed: true,
                    motion: false,
                    row,
                    col,
                    modifiers: event.modifiers,
                }
            }
            MouseEventKind::Released(button) => {
                self.pressed_button = None;
                MouseReport {
                    button_code: mouse_button_code(button)?,
                    pressed: false,
                    motion: false,
                    row,
                    col,
                    modifiers: event.modifiers,
                }
            }
            MouseEventKind::Moved => {
                if modes.contains(&TerminalMode::MouseAllMotion) {
                    MouseReport {
                        button_code: self.pressed_button.and_then(mouse_button_code).unwrap_or(3),
                        pressed: self.pressed_button.is_some(),
                        motion: true,
                        row,
                        col,
                        modifiers: event.modifiers,
                    }
                } else if modes.contains(&TerminalMode::MouseCellMotion)
                    && self.pressed_button.is_some()
                {
                    MouseReport {
                        button_code: self.pressed_button.and_then(mouse_button_code)?,
                        pressed: true,
                        motion: true,
                        row,
                        col,
                        modifiers: event.modifiers,
                    }
                } else {
                    return None;
                }
            }
            MouseEventKind::Wheel(delta) => {
                let (delta_x, delta_y) = scroll_delta_components(delta);
                let button_code = if delta_y > 0.0 {
                    64
                } else if delta_y < 0.0 {
                    65
                } else if delta_x > 0.0 {
                    66
                } else if delta_x < 0.0 {
                    67
                } else {
                    return None;
                };
                MouseReport {
                    button_code,
                    pressed: true,
                    motion: false,
                    row,
                    col,
                    modifiers: event.modifiers,
                }
            }
        };

        Some(encode_mouse_report(
            report,
            MouseEncoding::from_modes(modes),
        ))
    }
}

#[derive(Debug, Clone, Copy)]
struct MouseReport {
    button_code: u16,
    pressed: bool,
    motion: bool,
    row: u16,
    col: u16,
    modifiers: KeyModifiers,
}

fn mouse_button_code(button: MouseButton) -> Option<u16> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseEncoding {
    Legacy,
    Utf8,
    Sgr,
    Urxvt,
}

impl MouseEncoding {
    fn from_modes(modes: &BTreeSet<TerminalMode>) -> Self {
        if modes.contains(&TerminalMode::SgrMouse) {
            Self::Sgr
        } else if modes.contains(&TerminalMode::UrxvtMouse) {
            Self::Urxvt
        } else if modes.contains(&TerminalMode::Utf8Mouse) {
            Self::Utf8
        } else {
            Self::Legacy
        }
    }
}

fn encode_mouse_report(report: MouseReport, encoding: MouseEncoding) -> Vec<u8> {
    let mut button_code = report.button_code;
    if report.motion {
        button_code += 32;
    }
    if report.modifiers.shift {
        button_code += 4;
    }
    if report.modifiers.alt {
        button_code += 8;
    }
    if report.modifiers.ctrl {
        button_code += 16;
    }

    let legacy_code = if report.pressed { button_code } else { 3 };
    match encoding {
        MouseEncoding::Sgr => {
            let suffix = if report.pressed { 'M' } else { 'm' };
            format!(
                "\x1b[<{};{};{}{}",
                button_code, report.col, report.row, suffix
            )
            .into_bytes()
        }
        MouseEncoding::Urxvt => format!(
            "\x1b[{};{};{}M",
            legacy_code.saturating_add(32),
            report.col,
            report.row
        )
        .into_bytes(),
        MouseEncoding::Utf8 => {
            let mut bytes = b"\x1b[M".to_vec();
            push_utf8_mouse_value(&mut bytes, legacy_code);
            push_utf8_mouse_value(&mut bytes, report.col);
            push_utf8_mouse_value(&mut bytes, report.row);
            bytes
        }
        MouseEncoding::Legacy => vec![
            0x1b,
            b'[',
            b'M',
            encode_legacy_mouse_coord(legacy_code),
            encode_legacy_mouse_coord(report.col),
            encode_legacy_mouse_coord(report.row),
        ],
    }
}

fn push_utf8_mouse_value(output: &mut Vec<u8>, value: u16) {
    let value = u32::from(value).saturating_add(32).min(0x10ffff);
    let scalar = char::from_u32(value).unwrap_or('\u{fffd}');
    let mut encoded = [0; 4];
    output.extend_from_slice(scalar.encode_utf8(&mut encoded).as_bytes());
}

fn encode_legacy_mouse_coord(value: u16) -> u8 {
    value.saturating_add(32).min(255) as u8
}

fn ansi_color(index: u8, config: &AppConfig) -> RenderColor {
    const PALETTE: [RenderColor; 16] = [
        RenderColor::rgb(12, 12, 12),
        RenderColor::rgb(197, 15, 31),
        RenderColor::rgb(19, 161, 14),
        RenderColor::rgb(193, 156, 0),
        RenderColor::rgb(0, 55, 218),
        RenderColor::rgb(136, 23, 152),
        RenderColor::rgb(58, 150, 221),
        RenderColor::rgb(204, 204, 204),
        RenderColor::rgb(118, 118, 118),
        RenderColor::rgb(231, 72, 86),
        RenderColor::rgb(22, 198, 12),
        RenderColor::rgb(249, 241, 165),
        RenderColor::rgb(59, 120, 255),
        RenderColor::rgb(180, 0, 158),
        RenderColor::rgb(97, 214, 214),
        RenderColor::rgb(242, 242, 242),
    ];

    if index < 16 {
        return config
            .colors
            .palette
            .get(usize::from(index))
            .copied()
            .map(render_color)
            .or_else(|| PALETTE.get(usize::from(index)).copied())
            .unwrap_or(PALETTE[7]);
    }
    if index < 232 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let cube = index - 16;
        return RenderColor::rgb(
            LEVELS[usize::from(cube / 36)],
            LEVELS[usize::from((cube / 6) % 6)],
            LEVELS[usize::from(cube % 6)],
        );
    }
    let gray = 8u8.saturating_add((index - 232).saturating_mul(10));
    RenderColor::rgb(gray, gray, gray)
}
