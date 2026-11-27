//! Keeps Discord's access token between runs, so that the user is asked for permission once.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Stored {
    access_token: String,
}

/// A file in the application's data folder. The token lets its holder do what the bridge was
/// allowed to, so it stays on this computer and is never sent to the pages.
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            path: directory.join("discord.json"),
        }
    }

    pub fn load(&self) -> Option<String> {
        let stored: Stored = serde_json::from_slice(&fs::read(&self.path).ok()?).ok()?;
        let token = stored.access_token.trim();
        (!token.is_empty()).then(|| token.to_owned())
    }

    /// Without the file the bridge still works, and asks for permission again next time.
    pub fn save(&self, access_token: &str) {
        let stored = Stored {
            access_token: access_token.to_owned(),
        };
        let written = self
            .path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&self.path, serde_json::to_vec(&stored).unwrap_or_default()));
        if let Err(error) = written {
            eprintln!(
                "Could not save Discord's token to {}: {error}",
                self.path.display()
            );
        }
    }

    pub fn clear(&self) {
        let _ = fs::remove_file(&self.path);
    }
}
