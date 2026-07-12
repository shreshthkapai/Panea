//! Built-in themes, cursors, icons, and integration scripts.

pub const LAYER: &str = "visual overlay";

pub const PANEA_ICON_PNG_512: &[u8] = include_bytes!("../branding/generated/panea-icon-512.png");
pub const PANEA_ICON_ICO: &[u8] = include_bytes!("../branding/generated/panea.ico");
pub const PANEA_ICON_ICNS: &[u8] = include_bytes!("../branding/generated/Panea.icns");

pub struct ConfigExample {
    pub name: &'static str,
    pub contents: &'static str,
}

pub const CONFIG_EXAMPLES: &[ConfigExample] = &[
    ConfigExample {
        name: "plain-fast.toml",
        contents: include_str!("../config-examples/plain-fast.toml"),
    },
    ConfigExample {
        name: "balanced.toml",
        contents: include_str!("../config-examples/balanced.toml"),
    },
    ConfigExample {
        name: "command-blocks.toml",
        contents: include_str!("../config-examples/command-blocks.toml"),
    },
    ConfigExample {
        name: "minimal-aesthetic.toml",
        contents: include_str!("../config-examples/minimal-aesthetic.toml"),
    },
    ConfigExample {
        name: "heavy-visual-demo.toml",
        contents: include_str!("../config-examples/heavy-visual-demo.toml"),
    },
    ConfigExample {
        name: "foundational-customization.toml",
        contents: include_str!("../config-examples/foundational-customization.toml"),
    },
];

pub const PROGRAMMABLE_CONFIG_EXAMPLES: &[ConfigExample] = &[ConfigExample {
    name: "advanced.panea",
    contents: include_str!("../config-examples/advanced.panea"),
}];

#[must_use]
pub fn config_example(name: &str) -> Option<&'static str> {
    CONFIG_EXAMPLES
        .iter()
        .find(|example| example.name == name)
        .map(|example| example.contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_examples_are_shipped() {
        assert_eq!(CONFIG_EXAMPLES.len(), 6);
        assert!(config_example("balanced.toml").is_some());
        assert!(config_example("foundational-customization.toml").is_some());
        assert_eq!(PROGRAMMABLE_CONFIG_EXAMPLES.len(), 1);
    }

    #[test]
    fn application_icons_have_expected_container_signatures() {
        assert_eq!(&PANEA_ICON_PNG_512[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&PANEA_ICON_ICO[..4], &[0, 0, 1, 0]);
        assert_eq!(&PANEA_ICON_ICNS[..4], b"icns");
    }
}
