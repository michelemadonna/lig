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
pub enum ShowOn {
    #[default]
    Always,
    KeyToggle,
    OnHover,
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
pub enum Animation {
    #[default]
    None,
    Fade,
    Reveal,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    pub max_lines: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_numbers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub syntax_highlighting: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_protocol: Option<ImageProtocol>,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            command: Vec::new(),
            interpreter: None,
            script: None,
            fallback: Fallback::default(),
            output: Output::default(),
            max_lines: 200,
            line_numbers: None,
            syntax_highlighting: None,
            image_protocol: None,
        }
    }
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
    pub show_on: ShowOn,
    /// Delay before showing an unchanged selection in `on_hover` mode.
    pub on_hover_open_delay: u64,
    /// Visual transition when a preview becomes visible.
    pub animation: Animation,
    /// Duration of preview entrance transitions in milliseconds; 0 disables animation.
    pub animation_duration_ms: u64,
    pub layout: Layout,
    pub width: Size,
    pub height: Size,
    pub max_width: Size,
    pub max_height: Size,
    pub min_width: u16,
    pub min_height: u16,
    /// Distance between floating previews and the terminal edge, in cells.
    pub screen_margin: u16,
    pub margin: u16,
    pub adaptive_size: bool,
    pub scroll_step: usize,
    pub max_bytes: usize,
    pub timeout_ms: u64,
    pub cache_entries: usize,
    #[serde(rename = "preserve_completion", skip_serializing)]
    pub(super) legacy_preserve_completion: bool,
    pub preserve_prompt: bool,
    pub mouse: bool,
    pub show_title: bool,
    #[serde(rename = "show_controls", skip_serializing)]
    pub(super) legacy_show_controls: bool,
    pub keys: Keys,
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
            show_on: ShowOn::Always,
            on_hover_open_delay: 500,
            animation: Animation::None,
            animation_duration_ms: 300,
            layout: Layout::Auto,
            width: Size::Text("40%".into()),
            height: Size::Text("60%".into()),
            max_width: Size::Text("auto".into()),
            max_height: Size::Text("auto".into()),
            min_width: 28,
            min_height: 5,
            screen_margin: 10,
            margin: 1,
            adaptive_size: true,
            scroll_step: 3,
            max_bytes: 262_144,
            timeout_ms: 2000,
            cache_entries: 16,
            legacy_preserve_completion: true,
            preserve_prompt: true,
            mouse: true,
            show_title: true,
            legacy_show_controls: true,
            keys: Keys::default(),
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
                Size::Text(s) => s
                    .trim()
                    .strip_suffix('%')
                    .and_then(|n| n.parse::<f32>().ok())
                    .is_some_and(|n| n > 0.0 && n <= 100.0),
            };
            if !valid {
                return Err(format!("preview.{name} must be positive cells or 1–100%"));
            }
        }
        for (name, value) in [("max_width", &self.max_width), ("max_height", &self.max_height)] {
            let valid = match value {
                Size::Cells(n) => *n > 0,
                Size::Text(s) if s.trim() == "auto" => true,
                Size::Text(s) => s
                    .trim()
                    .strip_suffix('%')
                    .and_then(|n| n.parse::<f32>().ok())
                    .is_some_and(|n| n > 0.0 && n <= 100.0),
            };
            if !valid {
                return Err(format!("preview.{name} must be positive cells, 1–100%, or 'auto'"));
            }
        }
        if !(4..=256).contains(&self.min_width)
            || !(3..=128).contains(&self.min_height)
            || self.margin > 40
            || self.screen_margin > 40
        {
            return Err("preview requires min_width 4–256, min_height 3–128 and margins <= 40".into());
        }
        if !(1..=1_048_576).contains(&self.max_bytes) {
            return Err("preview.max_bytes must be 1–1048576".into());
        }
        if !(1..=30_000).contains(&self.timeout_ms) || self.cache_entries > 64 {
            return Err("preview.timeout_ms must be 1–30000 and cache_entries must be 0–64".into());
        }
        if self.on_hover_open_delay > 30_000 {
            return Err("preview.on_hover_open_delay must be 0–30000".into());
        }
        if self.animation_duration_ms > 30_000 {
            return Err("preview.animation_duration_ms must be 0–30000".into());
        }
        if !(1..=128).contains(&self.scroll_step) {
            return Err("preview.scroll_step must be 1–128".into());
        }
        for (name, engine) in [
            ("text", &self.text),
            ("image", &self.image),
            ("archive", &self.archive),
            ("media", &self.media),
            ("directory", &self.directory),
        ] {
            validate_engine(name, engine)?;
        }
        for (ext, engine) in &self.rules {
            validate_engine(&format!("rules.{ext}"), engine)?;
        }
        for (name, engine) in
            [("image", &self.image), ("archive", &self.archive), ("media", &self.media), ("directory", &self.directory)]
        {
            if engine.line_numbers.is_some() || engine.syntax_highlighting.is_some() {
                return Err(format!("preview.{name} does not support text styling options"));
            }
        }
        for (name, engine) in
            [("text", &self.text), ("archive", &self.archive), ("media", &self.media), ("directory", &self.directory)]
        {
            if engine.image_protocol.is_some() {
                return Err(format!("preview.{name}.image_protocol is only supported under preview.image"));
            }
        }
        for (name, engine) in &self.rules {
            if engine.line_numbers.is_some() || engine.syntax_highlighting.is_some() || engine.image_protocol.is_some()
            {
                return Err(format!("preview.rules.{name} does not support provider-specific display options"));
            }
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

fn validate_engine(name: &str, engine: &Engine) -> Result<(), String> {
    if !(1..=4096).contains(&engine.max_lines) {
        return Err(format!("preview.{name}.max_lines must be 1–4096"));
    }
    Ok(())
}
