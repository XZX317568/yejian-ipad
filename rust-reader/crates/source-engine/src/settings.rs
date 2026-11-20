//! User-editable source settings described by a source's `settings.json`.
//! Values live in the process-wide `defaults` store the WASM source reads.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::SourceManifest;
use crate::wasm::host_defaults::{self, DefaultValue};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SettingValue {
    Bool(bool),
    Int(i32),
    Float(f32),
    Str(String),
    StrList(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Toggle,
    Select {
        values: Vec<String>,
        titles: Vec<String>,
    },
    MultiSelect {
        values: Vec<String>,
        titles: Vec<String>,
    },
    Text {
        placeholder: String,
    },
    Stepper {
        min: f64,
        max: f64,
        step: f64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingItem {
    pub key: String,
    pub title: String,
    pub group: String,
    pub kind: SettingKind,
    pub default: Option<SettingValue>,
}

impl From<SettingValue> for DefaultValue {
    fn from(value: SettingValue) -> Self {
        match value {
            SettingValue::Bool(v) => DefaultValue::Bool(v),
            SettingValue::Int(v) => DefaultValue::Int(v),
            SettingValue::Float(v) => DefaultValue::Float(v),
            SettingValue::Str(v) => DefaultValue::Str(v),
            SettingValue::StrList(v) => DefaultValue::StrList(v),
        }
    }
}

/// Setting items a user can edit: the manifest's language and mirror
/// choices, then every supported item of `settings.json`.
pub fn source_settings(manifest: &SourceManifest, settings_json: Option<&str>) -> Vec<SettingItem> {
    let mut items = Vec::new();
    if manifest.languages.len() > 1 {
        items.push(SettingItem {
            key: "languages".into(),
            title: "语言".into(),
            group: String::new(),
            kind: SettingKind::MultiSelect {
                values: manifest.languages.clone(),
                titles: manifest.languages.clone(),
            },
            default: Some(SettingValue::StrList(manifest.languages.clone())),
        });
    }
    if manifest.allows_base_url_select && manifest.urls.len() > 1 {
        items.push(SettingItem {
            key: "url".into(),
            title: "站点地址".into(),
            group: String::new(),
            kind: SettingKind::Select {
                values: manifest.urls.clone(),
                titles: manifest.urls.clone(),
            },
            default: Some(SettingValue::Str(manifest.urls[0].clone())),
        });
    }
    if let Some(value) = settings_json.and_then(|json| serde_json::from_str::<Value>(json).ok()) {
        collect(&value, "", &mut items);
    }
    items
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn collect(value: &Value, group: &str, out: &mut Vec<SettingItem>) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| collect(item, group, out)),
        Value::Object(map) => {
            let title = map
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(items) = map.get("items").and_then(Value::as_array) {
                let group = if title.is_empty() { group } else { &title };
                items.iter().for_each(|item| collect(item, group, out));
            }
            let Some(key) = map
                .get("key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|k| !k.is_empty())
            else {
                return;
            };
            let ty = map.get("type").and_then(Value::as_str).unwrap_or("group");
            let default = map.get("default");
            let values = strings(map.get("values"));
            let mut titles = strings(map.get("titles"));
            if titles.len() != values.len() {
                titles = values.clone();
            }
            let number = |name: &str, fallback: f64| {
                map.get(name).and_then(Value::as_f64).unwrap_or(fallback)
            };
            let (kind, default) = match ty {
                "switch" | "toggle" => (
                    SettingKind::Toggle,
                    default.and_then(Value::as_bool).map(SettingValue::Bool),
                ),
                "select" | "picker" | "segment" if !values.is_empty() => (
                    SettingKind::Select { values, titles },
                    default
                        .and_then(Value::as_str)
                        .map(|v| SettingValue::Str(v.into())),
                ),
                "multi-select" if !values.is_empty() => (
                    SettingKind::MultiSelect { values, titles },
                    Some(SettingValue::StrList(strings(default))),
                ),
                "text" => (
                    SettingKind::Text {
                        placeholder: map
                            .get("placeholder")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    },
                    default
                        .and_then(Value::as_str)
                        .map(|v| SettingValue::Str(v.into())),
                ),
                "stepper" => (
                    SettingKind::Stepper {
                        min: number("minimumValue", 0.),
                        max: number("maximumValue", 100.),
                        step: number("stepValue", 1.).max(0.01),
                    },
                    default.and_then(Value::as_f64).map(|n| {
                        if n.fract() == 0.0 {
                            SettingValue::Int(n as i32)
                        } else {
                            SettingValue::Float(n as f32)
                        }
                    }),
                ),
                _ => return,
            };
            out.push(SettingItem {
                key: key.into(),
                title: if title.is_empty() { key.into() } else { title },
                group: group.into(),
                kind,
                default,
            });
        }
        _ => {}
    }
}

/// Current value as the source sees it (`None` until set or seeded).
pub fn setting_value(source_id: &str, key: &str) -> Option<SettingValue> {
    host_defaults::get_value(source_id, key).and_then(|value| match value {
        DefaultValue::Bool(v) => Some(SettingValue::Bool(v)),
        DefaultValue::Int(v) => Some(SettingValue::Int(v)),
        DefaultValue::Float(v) => Some(SettingValue::Float(v)),
        DefaultValue::Str(v) => Some(SettingValue::Str(v)),
        DefaultValue::StrList(v) => Some(SettingValue::StrList(v)),
        DefaultValue::Bytes(_) => None,
    })
}

/// Stores a user's choice for one source; it wins over seeded defaults.
pub fn set_setting(source_id: &str, key: &str, value: SettingValue) {
    host_defaults::set_value(&format!("{source_id}.{key}"), Some(value.into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_json_items_become_editable_and_override_defaults() {
        let json = r#"[{"type":"group","title":"阅读","items":[
            {"type":"select","key":"quality","title":"画质","values":["hd","sd"],"titles":["高清","标清"],"default":"sd"},
            {"type":"switch","key":"nsfw","title":"显示成人内容","default":false},
            {"type":"button","key":"logout","title":"退出"}]}]"#;
        let manifest = SourceManifest {
            id: "test.settings".into(),
            name: "T".into(),
            version: 1,
            alt_names: vec![],
            languages: vec!["zh".into()],
            urls: vec![],
            content_rating: 0,
            listings: vec![],
            allows_base_url_select: false,
            hides_filters_while_searching: false,
        };
        let items = source_settings(&manifest, Some(json));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].group, "阅读");
        assert_eq!(items[0].default, Some(SettingValue::Str("sd".into())));
        assert!(
            matches!(&items[0].kind, SettingKind::Select { titles, .. } if titles[0] == "高清")
        );
        host_defaults::seed_value("test.settings", "quality", DefaultValue::Str("sd".into()));
        set_setting("test.settings", "quality", SettingValue::Str("hd".into()));
        assert_eq!(
            setting_value("test.settings", "quality"),
            Some(SettingValue::Str("hd".into()))
        );
    }
}
