use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_kit_assets::AllAssets;

/// The Lucide icons plus the mascot drawings in `assets/mascot`.
pub struct Assets;

/// The `mascot/<name>.svg` drawings, each embedded from `assets/mascot`.
macro_rules! mascots {
    ($($name:literal),* $(,)?) => {
        &[$((
            concat!("mascot/", $name, ".svg"),
            include_bytes!(concat!("../assets/mascot/", $name, ".svg")),
        )),*]
    };
}

const MASCOTS: &[(&str, &[u8])] = mascots![
    "sobbing", "side-eye", "sad", "asleep", "angry", "cool", "dizzy", "glitch", "happy", "love",
    "sing", "thinking",
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match MASCOTS.iter().find(|(name, _)| *name == path) {
            Some((_, svg)) => Ok(Some(Cow::Borrowed(svg))),
            None => AllAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = AllAssets.list(path)?;
        names.extend(
            MASCOTS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::mascot;

    #[test]
    fn every_mascot_shown_is_embedded() {
        let shown = [
            mascot::WELCOMERS,
            mascot::GREETERS,
            mascot::IDLE,
            mascot::LONELY,
            mascot::UPSET,
        ];
        for path in shown.concat() {
            assert!(MASCOTS.iter().any(|(name, _)| *name == path), "{path}");
        }
    }
}
