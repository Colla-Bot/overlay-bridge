use std::{env, fs, path::Path};

const SETTINGS: [&str; 3] = [
    "COLLABOT_BRIDGE_DISCORD_CLIENT_ID",
    "COLLABOT_BRIDGE_DISCORD_ORIGIN",
    "COLLABOT_BRIDGE_DISCORD_TOKEN_URL",
];

/// Reads a setting from the text of a `.env` file: lines of `NAME=value`, of which blank ones and
/// those starting with `#` say nothing. A value may stand in quotes.
fn read<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().trim_matches(['"', '\'']))
}

fn main() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    println!("cargo:rerun-if-changed={}", file.display());
    let written = fs::read_to_string(&file).unwrap_or_default();

    for name in SETTINGS {
        println!("cargo:rerun-if-env-changed={name}");
        let given = env::var(name).ok();
        let value = given
            .as_deref()
            .or_else(|| read(&written, name))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(value) = value else {
            panic!(
                "{name} is not set. The bridge needs a Discord application to sign in as: \
                 copy bridge/.env.example to bridge/.env and fill it in, or set the variable. \
                 See \"The Discord application\" in bridge/README.md."
            );
        };
        // What `env!` in config.rs reads.
        println!("cargo:rustc-env={name}={value}");
    }

    tauri_build::build();
}
