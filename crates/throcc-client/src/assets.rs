use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_kit_assets::AllAssets;

/// The Lucide icons plus the mascot drawings in `assets/mascot`.
pub struct Assets;

const MASCOTS: &[(&str, &[u8])] = &[
    (
        "mascot/angry.svg",
        include_bytes!("../assets/mascot/angry.svg"),
    ),
    (
        "mascot/cool.svg",
        include_bytes!("../assets/mascot/cool.svg"),
    ),
    (
        "mascot/dizzy.svg",
        include_bytes!("../assets/mascot/dizzy.svg"),
    ),
    (
        "mascot/glitch.svg",
        include_bytes!("../assets/mascot/glitch.svg"),
    ),
    (
        "mascot/happy.svg",
        include_bytes!("../assets/mascot/happy.svg"),
    ),
    (
        "mascot/love.svg",
        include_bytes!("../assets/mascot/love.svg"),
    ),
    (
        "mascot/sing.svg",
        include_bytes!("../assets/mascot/sing.svg"),
    ),
    (
        "mascot/thinking.svg",
        include_bytes!("../assets/mascot/thinking.svg"),
    ),
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
