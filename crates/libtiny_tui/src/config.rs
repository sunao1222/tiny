use libtiny_common::{ChanName, ChanNameRef};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use std::collections::HashMap;
use std::path::Path;

use ratatui::style::{Color, Modifier};

use crate::key_map::KeyMap;
use crate::notifier::Notifier;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Config {
    pub(crate) servers: Vec<Server>,

    pub(crate) defaults: Defaults,

    #[serde(default)]
    pub(crate) colors: Colors,

    #[serde(default = "default_scrollback")]
    pub(crate) scrollback: usize,

    pub(crate) layout: Option<Layout>,

    #[serde(default = "default_max_nick_length")]
    pub(crate) max_nick_length: usize,

    #[serde(default)]
    pub(crate) key_map: Option<KeyMap>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct Server {
    pub(crate) addr: String,
    pub(crate) join: Vec<Chan>,
    #[serde(flatten)]
    pub(crate) config: TabConfig,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct Defaults {
    #[serde(default, flatten)]
    pub(crate) tab_config: TabConfig,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            tab_config: TabConfig {
                ignore: Some(false),
                notify: Some(Notifier::default()),
            },
        }
    }
}

#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum Chan {
    #[serde(deserialize_with = "deser_chan_name")]
    Name(ChanName),
    WithConfig {
        #[serde(deserialize_with = "deser_chan_name")]
        name: ChanName,
        #[serde(flatten)]
        config: TabConfig,
    },
}

impl Chan {
    pub fn from_cmd_args(s: &str) -> Result<Chan, String> {
        // Make sure channel starts with '#'
        let s = if !s.starts_with('#') {
            format!("#{s}")
        } else {
            s.to_string()
        };
        // Try to split chan name and args
        match s.split_once(' ') {
            // with args
            Some((name, args)) => {
                let config = TabConfig::from_cmd_args(args)?;
                Ok(Chan::WithConfig {
                    name: ChanName::new(name.to_string()),
                    config,
                })
            }
            // chan name only
            None => Ok(Chan::Name(ChanName::new(s))),
        }
    }

    pub fn name(&self) -> &ChanNameRef {
        match self {
            Chan::Name(name) => name,
            Chan::WithConfig { name, .. } => name,
        }
        .as_ref()
    }
}

fn deser_chan_name<'de, D>(d: D) -> Result<ChanName, D::Error>
where
    D: Deserializer<'de>,
{
    let name: String = serde::de::Deserialize::deserialize(d)?;
    Ok(ChanName::new(name))
}

/// Map of TabConfigs by tab names
#[derive(Debug, Default)]
pub(crate) struct TabConfigs(HashMap<String, TabConfig>);

impl TabConfigs {
    pub(crate) fn get(
        &self,
        serv_name: &str,
        chan_name: Option<&ChanNameRef>,
    ) -> Option<TabConfig> {
        let key = if let Some(chan) = chan_name {
            format!("{}_{}", serv_name, chan.display())
        } else {
            serv_name.to_string()
        };
        self.0.get(&key).cloned()
    }

    pub(crate) fn get_mut(
        &mut self,
        serv_name: &str,
        chan_name: Option<&ChanNameRef>,
    ) -> Option<&mut TabConfig> {
        let key = if let Some(chan) = chan_name {
            format!("{}_{}", serv_name, chan.display())
        } else {
            serv_name.to_string()
        };
        self.0.get_mut(&key)
    }

    pub(crate) fn set(
        &mut self,
        serv_name: &str,
        chan_name: Option<&ChanNameRef>,
        config: TabConfig,
    ) {
        let key = if let Some(chan) = chan_name {
            format!("{}_{}", serv_name, chan.display())
        } else {
            serv_name.to_string()
        };
        self.0.insert(key, config);
    }

    pub(crate) fn set_by_server(&mut self, serv_name: &str, config: TabConfig) {
        for c in self
            .0
            .iter_mut()
            .filter(|entry| entry.0.starts_with(serv_name))
        {
            *c.1 = config;
        }
    }
}

impl From<&Config> for TabConfigs {
    fn from(config: &Config) -> Self {
        let mut tab_configs = HashMap::new();
        for server in &config.servers {
            let serv_tc = server.config.or_use(&config.defaults.tab_config);
            tab_configs.insert(server.addr.clone(), serv_tc);
            for chan in &server.join {
                let (name, tc) = match chan {
                    Chan::Name(name) => (name, serv_tc),
                    Chan::WithConfig { name, config } => (name, config.or_use(&serv_tc)),
                };
                tab_configs.insert(format!("{}_{}", server.addr, name.display()), tc);
            }
        }
        tab_configs.insert("_defaults".to_string(), config.defaults.tab_config);
        debug!("new {tab_configs:?}");
        Self(tab_configs)
    }
}

#[derive(Debug, Default, Copy, Clone, Deserialize, PartialEq, Eq)]
pub struct TabConfig {
    /// Whether the join/part messages are ignored.
    #[serde(default)]
    pub ignore: Option<bool>,

    /// Notification setting for tab.
    #[serde(default)]
    pub notify: Option<Notifier>,
}

impl TabConfig {
    pub(crate) fn from_cmd_args(s: &str) -> Result<TabConfig, String> {
        let mut config = TabConfig::default();
        let mut words = s.trim().split(' ').map(str::trim);

        while let Some(word) = words.next() {
            // `"".split(' ')` yields one empty string.
            if word.is_empty() {
                continue;
            }
            match word {
                "-ignore" => config.ignore = Some(true),
                "-notify" => match words.next() {
                    Some(notify_setting) => {
                        config.notify = Some(Notifier::from_cmd_args(notify_setting)?)
                    }
                    None => return Err("-notify parameter missing".to_string()),
                },
                other => return Err(format!("Unexpected channel parameter: {other:?}")),
            }
        }

        Ok(config)
    }

    pub(crate) fn or_use(&self, config: &TabConfig) -> TabConfig {
        TabConfig {
            ignore: self.ignore.or(config.ignore),
            notify: self.notify.or(config.notify),
        }
    }

    pub(crate) fn toggle_ignore(&mut self) -> bool {
        let ignore = self.ignore.get_or_insert(false);
        *ignore = !&*ignore;
        *ignore
    }
}

fn default_scrollback() -> usize {
    usize::MAX
}

fn default_max_nick_length() -> usize {
    12
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub modifiers: Modifier,
}

impl From<Style> for ratatui::style::Style {
    fn from(style: Style) -> Self {
        Self::default()
            .fg(style.fg)
            .bg(style.bg)
            .add_modifier(style.modifiers)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Layout {
    Compact,
    Aligned,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Colors {
    pub nick: Vec<u8>,
    pub clear: Style,
    pub user_msg: Style,
    pub err_msg: Style,
    pub topic: Style,
    pub cursor: Style,
    pub join: Style,
    pub part: Style,
    pub nick_change: Style,
    pub faded: Style,
    pub exit_dialogue: Style,
    pub highlight: Style,
    pub completion: Style,
    pub timestamp: Style,
    pub tab_active: Style,
    pub tab_normal: Style,
    pub tab_new_msg: Style,
    pub tab_highlight: Style,
    pub tab_joinpart: Style,
}

impl Default for Colors {
    fn default() -> Self {
        Colors {
            nick: vec![1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14],
            clear: style(0, 0, Modifier::empty()),
            user_msg: style(0, 0, Modifier::empty()),
            err_msg: style(0, 1, Modifier::BOLD),
            topic: style(14, 0, Modifier::BOLD),
            cursor: style(0, 0, Modifier::empty()),
            join: style(10, 0, Modifier::BOLD),
            part: style(1, 0, Modifier::BOLD),
            nick_change: style(10, 0, Modifier::BOLD),
            faded: style(242, 0, Modifier::empty()),
            exit_dialogue: style(0, 4, Modifier::empty()),
            highlight: style(9, 0, Modifier::BOLD),
            completion: style(84, 0, Modifier::empty()),
            timestamp: style(242, 0, Modifier::empty()),
            tab_active: style(0, 0, Modifier::BOLD),
            tab_normal: style(8, 0, Modifier::empty()),
            tab_new_msg: style(5, 0, Modifier::empty()),
            tab_highlight: style(9, 0, Modifier::BOLD),
            tab_joinpart: style(11, 0, Modifier::empty()),
        }
    }
}

fn indexed_color(color: u8) -> Color {
    if color == 0 {
        Color::Reset
    } else {
        Color::Indexed(color)
    }
}

fn style(fg: u8, bg: u8, modifiers: Modifier) -> Style {
    Style {
        fg: indexed_color(fg),
        bg: indexed_color(bg),
        modifiers,
    }
}

//
// Parsing
//

// Color names are taken from https://en.wikipedia.org/wiki/List_of_software_palettes
const COLORS: [(&str, u8); 17] = [
    ("default", 0), // Default fg/bg color of the terminal
    ("black", 0),
    ("maroon", 1),
    ("green", 2),
    ("olive", 3),
    ("navy", 4),
    ("purple", 5),
    ("teal", 6),
    ("silver", 7),
    ("gray", 8),
    ("red", 9),
    ("lime", 10),
    ("yellow", 11),
    ("blue", 12),
    ("magenta", 13),
    ("cyan", 14),
    ("white", 15),
];

const ATTRS: [(&str, Modifier); 4] = [
    ("bold", Modifier::BOLD),
    ("underline", Modifier::UNDERLINED),
    ("italic", Modifier::ITALIC),
    ("strikethrough", Modifier::CROSSED_OUT),
];

fn parse_color(val: String) -> Option<Color> {
    for &(name, color) in &COLORS {
        if val == name {
            return Some(indexed_color(color));
        }
    }

    // If color name doesn't match try get a number
    val.parse::<u8>().ok().map(indexed_color)
}

fn parse_attr(val: String) -> Modifier {
    for &(name, attr) in &ATTRS {
        if name == val {
            return attr;
        }
    }
    Modifier::empty()
}

impl<'de> Deserialize<'de> for Style {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Fg,
            Bg,
            Attrs,
        }

        use std::fmt;

        struct StyleVisitor;
        impl<'de> Visitor<'de> for StyleVisitor {
            type Value = Style;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                let colors = COLORS
                    .iter()
                    .map(|&(name, _)| name)
                    .collect::<Vec<&str>>()
                    .join(", ");
                let attrs = ATTRS
                    .iter()
                    .map(|&(name, _)| name)
                    .collect::<Vec<&str>>()
                    .join(", ");

                writeln!(
                    formatter,
                    "fg: 0-255 or color name\n\
                     bg: 0-255 or color name\n\
                     attrs: [{attrs}]\n\n\
                     color names: {colors}"
                )
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut fg: Option<Color> = None;
                let mut bg: Option<Color> = None;
                let mut attr = Modifier::empty();

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Fg => {
                            let color = parse_color(map.next_value()?).ok_or_else(|| {
                                de::Error::invalid_value(de::Unexpected::UnitVariant, &self)
                            })?;

                            fg = Some(color);
                        }

                        Field::Bg => {
                            let color = parse_color(map.next_value()?).ok_or_else(|| {
                                de::Error::invalid_value(de::Unexpected::UnitVariant, &self)
                            })?;

                            bg = Some(color);
                        }

                        Field::Attrs => {
                            let attrs: Vec<String> = map.next_value()?;
                            attr = attrs
                                .into_iter()
                                .map(parse_attr)
                                .fold(Modifier::empty(), |style, a| style | a);
                        }
                    }
                }

                let fg = fg.ok_or_else(|| de::Error::missing_field("fg"))?;
                let bg = bg.ok_or_else(|| de::Error::missing_field("bg"))?;

                Ok(Style {
                    fg,
                    bg,
                    modifiers: attr,
                })
            }
        }

        d.deserialize_map(StyleVisitor)
    }
}

pub(crate) fn parse_config(config_path: &Path) -> Result<Config, serde_yaml::Error> {
    // tiny creates a config file with the defaults when it can't find one, but the config file can
    // be deleted before a `/reload`.
    let contents = std::fs::read_to_string(config_path).map_err(|err| {
        de::Error::custom(format!(
            "Can't read config file '{}': {}",
            config_path.to_string_lossy(),
            err
        ))
    })?;
    serde_yaml::from_str(&contents)
}

#[cfg(test)]
mod style_tests {
    use super::*;

    #[test]
    fn yaml_colors_and_modifiers_preserve_terminal_semantics() {
        let parsed: Style = serde_yaml::from_str(
            "fg: red\nbg: 255\nattrs: [bold, underline, italic, strikethrough]",
        )
        .unwrap();
        assert_eq!(parsed.fg, Color::Indexed(9));
        assert_eq!(parsed.bg, Color::Indexed(255));
        assert_eq!(
            parsed.modifiers,
            Modifier::BOLD | Modifier::UNDERLINED | Modifier::ITALIC | Modifier::CROSSED_OUT
        );

        let defaults: Style = serde_yaml::from_str("fg: 0\nbg: default").unwrap();
        assert_eq!(defaults.fg, Color::Reset);
        assert_eq!(defaults.bg, Color::Reset);
        assert_eq!(defaults.modifiers, Modifier::empty());
        assert!(serde_yaml::from_str::<Style>("fg: 256\nbg: 0").is_err());
    }
}
