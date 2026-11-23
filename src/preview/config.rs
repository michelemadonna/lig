use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{config::Size, keys::Key};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Auto,
    Inline,
    Floating,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageProtocol {
    #[default]
    Auto,
    Cells,
    HalfBlocks,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    #[default]
    Auto,
    Right,
    Left,
    Above,
    #[serde(alias = "bottom")]
    Below,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Native,
    Command,
    Shell,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    #[default]
    Native,
    Metadata,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Plain,
    MediainfoJson,
    SevenZipList,
    #[default]
    Ansi,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Engine {
    pub backend: Backend,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interpreter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    pub fallback: Fallback,
    pub output: Output,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    pub toggle: String,
    #[serde(default)]
    pub toggle_layout: Option<String>,
    #[serde(default)]
    pub resize_floating: Option<String>,
    pub open: String,
    pub close: String,
    pub up: String,
    pub down: String,
    pub page_up: String,
    pub page_down: String,
}

const DEFAULT_LAYOUT_KEY: &str = "alt-f";
const DEFAULT_RESIZE_KEY: &str = "alt-z";

impl Default for Keys {
    fn default() -> Self {
        Self {
            toggle: "alt-p".into(),
            toggle_layout: Some(DEFAULT_LAYOUT_KEY.into()),
            resize_floating: Some(DEFAULT_RESIZE_KEY.into()),
            open: String::new(),
            close: String::new(),
            up: "alt-up".into(),
            down: "alt-down".into(),
            page_up: "page-up".into(),
            page_down: "page-down".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub start_hidden: bool,
    pub layout: Layout,
    pub position: Position,
    pub width: Size,
    pub height: Size,
    pub min_width: u16,
    pub min_height: u16,
    pub margin: u16,
    pub adaptive_size: bool,
    pub max_lines: usize,
    pub scroll_step: usize,
    pub max_bytes: usize,
    pub timeout_ms: u64,
    pub cache_entries: usize,
    pub line_numbers: bool,
    pub syntax_highlighting: bool,
    #[serde(rename = "preserve_completion", skip_serializing)]
    pub(super) legacy_preserve_completion: bool,
    pub preserve_prompt: bool,
    pub mouse: bool,
    pub show_title: bool,
    #[serde(rename = "show_controls", skip_serializing)]
    pub(super) legacy_show_controls: bool,
    pub keys: Keys,
    pub image_protocol: ImageProtocol,
    pub text: Engine,
    pub image: Engine,
    pub archive: Engine,
    pub media: Engine,
    pub directory: Engine,
    pub rules: BTreeMap<String, Engine>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hidden: false,
            layout: Layout::Auto,
            position: Position::Auto,
            width: Size::Text("40%".into()),
            height: Size::Text("60%".into()),
            min_width: 28,
            min_height: 5,
            margin: 1,
            adaptive_size: true,
            max_lines: 200,
            scroll_step: 3,
            max_bytes: 262_144,
            timeout_ms: 2000,
            cache_entries: 16,
            line_numbers: true,
            syntax_highlighting: true,
            legacy_preserve_completion: true,
            preserve_prompt: true,
            mouse: true,
            show_title: true,
            legacy_show_controls: true,
            keys: Keys::default(),
            image_protocol: ImageProtocol::Auto,
            text: Engine::default(),
            image: Engine::default(),
            archive: Engine::default(),
            media: Engine::default(),
            directory: Engine::default(),
            rules: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn validate(&mut self) -> Result<(), String> {
        for (name, value) in [("width", &self.width), ("height", &self.height)] {
            let valid = match value {
                Size::Cells(n) => *n > 0,
                Size::Text(s) if s == "auto" => true,
                Size::Text(s) => {
                    s.strip_suffix('%').and_then(|n| n.parse::<f32>().ok()).is_some_and(|n| n > 0.0 && n <= 100.0)
                }
            };
            if !valid {
                return Err(format!("preview.{name} must be positive cells, 1–100%, or 'auto'"));
            }
        }
        if !(4..=256).contains(&self.min_width) || !(3..=128).contains(&self.min_height) || self.margin > 40 {
            return Err("preview requires min_width 4–256, min_height 3–128 and margin <= 40".into());
        }
        if !(1..=4096).contains(&self.max_lines) || !(1..=1_048_576).contains(&self.max_bytes) {
            return Err("preview.max_lines must be 1–4096 and max_bytes must be 1–1048576".into());
        }
        if !(1..=30_000).contains(&self.timeout_ms) || self.cache_entries > 64 {
            return Err("preview.timeout_ms must be 1–30000 and cache_entries must be 0–64".into());
        }
        if !(1..=128).contains(&self.scroll_step) {
            return Err("preview.scroll_step must be 1–128".into());
        }
        let mut keys = Vec::new();
        for (name, binding) in [
            ("toggle", self.keys.toggle.as_str()),
            ("toggle_layout", self.keys.toggle_layout.as_deref().unwrap_or_default()),
            ("resize_floating", self.keys.resize_floating.as_deref().unwrap_or_default()),
            ("open", self.keys.open.as_str()),
            ("close", self.keys.close.as_str()),
            ("up", self.keys.up.as_str()),
            ("down", self.keys.down.as_str()),
            ("page_up", self.keys.page_up.as_str()),
            ("page_down", self.keys.page_down.as_str()),
        ] {
            if binding.is_empty() {
                continue;
            }
            let Some(key) = Key::parse_name(binding) else {
                return Err(format!("unknown preview.keys.{name} binding {binding:?}"));
            };
            if keys.contains(&key) {
                return Err(format!("duplicate preview.keys.{name} binding {binding:?}"));
            }
            keys.push(key);
        }
        for (binding, default) in
            [(&mut self.keys.toggle_layout, DEFAULT_LAYOUT_KEY), (&mut self.keys.resize_floating, DEFAULT_RESIZE_KEY)]
        {
            if binding.is_none() {
                // A configured shortcut takes priority over a newly introduced default.
                let taken = Key::parse_name(default).is_some_and(|key| keys.contains(&key));
                *binding = Some(if taken { String::new() } else { default.into() });
            }
        }
        let mut rules = BTreeMap::new();
        for (name, engine) in std::mem::take(&mut self.rules) {
            let ext = name.trim_start_matches('.').to_lowercase();
            if ext.is_empty() || ext.contains(['/', '\\']) || rules.insert(ext, engine).is_some() {
                return Err(format!("invalid or duplicate preview extension rule {name:?}"));
            }
        }
        self.rules = rules;
        for (name, engine) in [
            ("text", &self.text),
            ("image", &self.image),
            ("archive", &self.archive),
            ("media", &self.media),
            ("directory", &self.directory),
        ]
        .into_iter()
        .chain(self.rules.iter().map(|(name, engine)| (name.as_str(), engine)))
        {
            match engine.backend {
                Backend::Command if engine.command.first().is_none_or(|exe| exe.trim().is_empty()) => {
                    return Err(format!("preview backend {name:?} requires a nonempty command array"));
                }
                Backend::Shell
                    if engine.script.as_ref().is_none_or(|script| script.trim().is_empty())
                        || engine.interpreter.as_ref().is_some_and(|exe| exe.trim().is_empty()) =>
                {
                    return Err(format!("preview backend {name:?} requires a script and a valid interpreter"));
                }
                _ => {}
            }
        }
        Ok(())
    }
}
