use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

pub(crate) struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/clipboard.svg" => Some(include_bytes!("../assets/icons/clipboard.svg")),
            "icons/coins.svg" => Some(include_bytes!("../assets/icons/coins.svg")),
            "icons/smile.svg" => Some(include_bytes!("../assets/icons/smile.svg")),
            "icons/hash.svg" => Some(include_bytes!("../assets/icons/hash.svg")),
            "icons/clock.svg" => Some(include_bytes!("../assets/icons/clock.svg")),
            "icons/file.svg" => Some(include_bytes!("../assets/icons/file.svg")),
            "icons/command.svg" => Some(include_bytes!("../assets/icons/command.svg")),
            "icons/calculator.svg" => Some(include_bytes!("../assets/icons/calculator.svg")),
            "icons/back.svg" => Some(include_bytes!("../assets/icons/back.svg")),
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
            "icons/browser.svg" => Some(include_bytes!("../assets/icons/browser.svg")),
            "icons/finder.svg" => Some(include_bytes!("../assets/icons/finder.svg")),
            "icons/google.svg" => Some(include_bytes!("../assets/icons/google.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
