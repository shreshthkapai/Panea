//! Built-in themes, cursors, icons, and integration scripts.

pub const LAYER: &str = "visual overlay";

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
        assert_eq!(CONFIG_EXAMPLES.len(), 5);
        assert!(config_example("balanced.toml").is_some());
        assert_eq!(PROGRAMMABLE_CONFIG_EXAMPLES.len(), 1);
    }
}
