//! Static `filters.json` parsing and resolved-default filter values, matching
//! AidokuRunner's `Source` and Aidoku's `FilterListSheetView` behaviour.
//!
//! Upstream `Source.init` decodes `filters.json` with a plain `try
//! decoder.decode(...)` — a malformed file fails source init there. For the
//! reader the contract is softer per the host brief: the source still loads
//! and searches, and the error surfaces only when the caller asks `filters()`
//! for the schema. `parse_filters_json` therefore returns a hard, descriptive
//! error and the engine stores it as a `Result`.
//!
//! Merge semantics upstream (`Source.getSearchFilters`) are append-only:
//! `staticFilters + dynamicFilters`, static first, no deduplication.
//! `getListings` merges the same way (`staticListings + dynamicListings`).

use serde_json::Value;

use crate::error::SourceError;
use crate::wasm::models::{
    Filter, FilterTypeConfig, FilterValue, MultiSelectFilterCfg, SelectFilterCfg, SortDefault,
    SortFilterValue,
};

/// Parses the contents of a source's `filters.json` into engine [`Filter`]s.
///
/// Returns `Err` describing the decode failure when the JSON is not an array
/// of valid Aidoku filter objects. Malformed optional fields (a non-string
/// `options` entry, a non-numeric `min`, a wrong-typed `canAscend`, …) are
/// reported, not silently dropped — matching AidokuRunner's strict decode.
/// An explicit JSON `null` for an *optional* key is treated as absent
/// (AidokuRunner uses `decodeIfPresent`, which accepts `null` as `nil`).
pub fn parse_filters_json(text: &str) -> Result<Vec<Filter>, SourceError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| SourceError::Deserialize(format!("filters.json is not valid JSON: {e}")))?;
    let items = value.as_array().ok_or_else(|| {
        SourceError::Deserialize("filters.json must be a JSON array of filters".into())
    })?;
    let mut filters = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        filters.push(
            filter_from_json(item).map_err(|e| {
                SourceError::Deserialize(format!("filters.json entry {index}: {e}"))
            })?,
        );
    }
    Ok(filters)
}

fn filter_from_json(item: &Value) -> Result<Filter, String> {
    let object = item
        .as_object()
        .ok_or_else(|| "filter is not an object".to_string())?;
    let ty =
        string_field(object, "type").ok_or_else(|| "filter has no string `type`".to_string())?;
    let title = string_field(object, "title").map(str::to_string);
    let hide_from_header = bool_field(object, "hideFromHeader")?;

    let config = match ty {
        "text" => FilterTypeConfig::Text {
            placeholder: string_field(object, "placeholder").map(str::to_string),
        },
        "sort" => {
            let can_ascend = bool_field(object, "canAscend")?.unwrap_or(true);
            let options = required_string_list(object, "options")?;
            let default_value = match object.get("default") {
                Some(Value::Null) | None => None,
                Some(v) => Some(sort_default_from_json(v)?),
            };
            FilterTypeConfig::Sort {
                can_ascend,
                options,
                default_value,
            }
        }
        "check" => {
            let name = string_field(object, "name").map(str::to_string);
            let can_exclude = bool_field(object, "canExclude")?.unwrap_or(false);
            let default_value = bool_field(object, "default")?;
            FilterTypeConfig::Check {
                name,
                can_exclude,
                default_value,
            }
        }
        "select" => {
            let is_genre = bool_field(object, "isGenre")?.unwrap_or(false);
            let uses_tag_style = bool_field(object, "usesTagStyle")?.unwrap_or(is_genre);
            let cfg = SelectFilterCfg {
                is_genre,
                uses_tag_style,
                options: required_string_list(object, "options")?,
                ids: optional_string_list(object, "ids")?,
                default_value: string_field(object, "default").map(str::to_string),
            };
            FilterTypeConfig::Select(cfg)
        }
        "multi-select" => {
            let is_genre = bool_field(object, "isGenre")?.unwrap_or(false);
            let cfg = MultiSelectFilterCfg {
                is_genre,
                can_exclude: bool_field(object, "canExclude")?.unwrap_or(false),
                uses_tag_style: bool_field(object, "usesTagStyle")?.unwrap_or(is_genre),
                options: required_string_list(object, "options")?,
                ids: optional_string_list(object, "ids")?,
                default_included: optional_string_list(object, "defaultIncluded")?,
                default_excluded: optional_string_list(object, "defaultExcluded")?,
            };
            FilterTypeConfig::MultiSelect(cfg)
        }
        "note" => FilterTypeConfig::Note(
            string_field(object, "text")
                .ok_or_else(|| "note filter has no `text`".to_string())?
                .to_string(),
        ),
        "range" => FilterTypeConfig::Range {
            min: finite_f32_field(object, "min")?,
            max: finite_f32_field(object, "max")?,
            decimal: bool_field(object, "decimal")?.unwrap_or(false),
        },
        other => return Err(format!("invalid filter type \"{other}\"")),
    };

    // Upstream: `self.id = id ?? title ?? type`
    let id = string_field(object, "id")
        .map(str::to_string)
        .or_else(|| title.clone())
        .unwrap_or_else(|| ty.to_string());

    Ok(Filter {
        id,
        title,
        hide_from_header,
        config,
    })
}

fn sort_default_from_json(value: &Value) -> Result<SortDefault, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "sort `default` is not an object".to_string())?;
    let index = object
        .get("index")
        .ok_or_else(|| "sort `default` has no `index`".to_string())?;
    let index = index
        .as_i64()
        .ok_or_else(|| "sort `default.index` is not an integer".to_string())?;
    let ascending = match object.get("ascending") {
        Some(Value::Null) | None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("sort `default.ascending` is not a bool".to_string()),
    };
    Ok(SortDefault { index, ascending })
}

// ---- strict field accessors ----------------------------------------------
// These accept a JSON `null` for any optional key as "absent" (matching
// `decodeIfPresent`) but reject the wrong JSON type instead of dropping it.

fn get<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a Value> {
    match object.get(key) {
        Some(Value::Null) | None => None,
        other => other,
    }
}

fn string_field<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    get(object, key).and_then(Value::as_str)
}

fn bool_field(object: &serde_json::Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match get(object, key) {
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("`{key}` is not a bool")),
        None => Ok(None),
    }
}

fn finite_f32_field(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<f32>, String> {
    match get(object, key) {
        Some(Value::Number(n)) => {
            let v = n
                .as_f64()
                .ok_or_else(|| format!("`{key}` is not a number"))?;
            if v.is_finite() {
                Ok(Some(v as f32))
            } else {
                Err(format!("`{key}` is not a finite number"))
            }
        }
        Some(_) => Err(format!("`{key}` is not a number")),
        None => Ok(None),
    }
}

fn required_string_list(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    let items = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{key}` is missing or not an array"))?;
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        out.push(
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("`{key}[{i}]` is not a string"))?,
        );
    }
    Ok(out)
}

fn optional_string_list(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, String> {
    match get(object, key) {
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                out.push(
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("`{key}[{i}]` is not a string"))?,
                );
            }
            Ok(Some(out))
        }
        Some(_) => Err(format!("`{key}` is not an array")),
        None => Ok(None),
    }
}

/// The resolved default [`FilterValue`] for each interactive filter, in schema
/// order. This is the *baseline* Aidoku's UI compares against: a value equal
/// to the baseline is treated as "unchanged" and is not sent to the source.
/// `note` filters are display-only and produce no entry.
///
/// Resolved defaults, matching Aidoku's `FilterListSheetView` / `resolvedDefaultValue`:
/// - `text`   → `Text{id, ""}` (empty is the unchanged baseline).
/// - `sort`   → `Sort{id, index, ascending}`; `default` absent → `index 0, ascending false`.
/// - `check`  → `Check{id, value}`; `default: true`→`1`, `default: false`→`2`,
///   `default` absent → `0` (tri-state: 0 neutral, 1 include, 2 exclude).
/// - `select` → `Select{id, value}` where value = `default ?? ids.first ?? options.first ?? ""`.
/// - `multi-select` → `MultiSelect{id, included ?? [], excluded ?? []}`.
/// - `range`  → `Range{id, from: None, to: None}` (an unset range is the baseline).
///
/// These are display/comparison baselines only — a submit equal to this list
/// means "no filters" and is emitted as an empty `filters` list by the UI.
pub fn default_filter_values(filters: &[Filter]) -> Vec<FilterValue> {
    filters.iter().filter_map(resolved_default).collect()
}

fn resolved_default(filter: &Filter) -> Option<FilterValue> {
    let id = filter.id.clone();
    match &filter.config {
        FilterTypeConfig::Text { .. } => Some(FilterValue::Text {
            id,
            value: String::new(),
        }),
        FilterTypeConfig::Sort { default_value, .. } => Some(FilterValue::Sort(SortFilterValue {
            id,
            index: default_value.as_ref().map(|d| d.index as i32).unwrap_or(0),
            ascending: default_value.as_ref().map(|d| d.ascending).unwrap_or(false),
        })),
        FilterTypeConfig::Check { default_value, .. } => Some(FilterValue::Check {
            id,
            value: match default_value {
                Some(true) => 1,
                Some(false) => 2,
                None => 0,
            },
        }),
        FilterTypeConfig::Select(cfg) => Some(FilterValue::Select {
            id,
            value: resolved_select_default(cfg),
        }),
        FilterTypeConfig::MultiSelect(cfg) => Some(FilterValue::MultiSelect {
            id,
            included: cfg.default_included.clone().unwrap_or_default(),
            excluded: cfg.default_excluded.clone().unwrap_or_default(),
        }),
        FilterTypeConfig::Range { .. } => Some(FilterValue::Range {
            id,
            from: None,
            to: None,
        }),
        FilterTypeConfig::Note(_) => None,
    }
}

fn resolved_select_default(cfg: &SelectFilterCfg) -> String {
    cfg.default_value
        .clone()
        .or_else(|| cfg.ids.as_ref().and_then(|ids| ids.first().cloned()))
        .or_else(|| cfg.options.first().cloned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_filters_json() {
        let json = r#"[
            {"id":"q","type":"text","title":"Search","placeholder":"title…"},
            {"id":"order","type":"sort","title":"Sort","canAscend":true,
             "options":["relevance","latest"],"default":{"index":1,"ascending":false}},
            {"id":"adult","type":"check","title":"Adult","name":"18+",
             "canExclude":true,"default":true},
            {"id":"status","type":"select","title":"Status","options":["any","ongoing"],
             "ids":["","1"],"default":"1"},
            {"id":"genre","type":"multi-select","title":"Genre","isGenre":true,
             "canExclude":true,"options":["action","romance"],
             "ids":["a","r"],"defaultIncluded":["a"],"defaultExcluded":["r"]},
            {"id":"note1","type":"note","text":"hello"},
            {"id":"year","type":"range","min":1990,"max":2030,"decimal":false},
            {"type":"text","title":"NoId"}
        ]"#;
        let filters = parse_filters_json(json).unwrap();
        assert_eq!(filters.len(), 8);
        assert_eq!(filters[7].id, "NoId");
        match &filters[1].config {
            FilterTypeConfig::Sort {
                can_ascend,
                options,
                default_value,
            } => {
                assert!(*can_ascend);
                assert_eq!(options.len(), 2);
                assert_eq!(
                    default_value.as_ref().unwrap(),
                    &SortDefault {
                        index: 1,
                        ascending: false
                    }
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        match &filters[4].config {
            FilterTypeConfig::MultiSelect(cfg) => {
                assert!(cfg.is_genre);
                assert_eq!(
                    cfg.default_included.as_deref(),
                    Some(&["a".to_string()][..])
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rejects_non_array_and_bad_type() {
        assert!(parse_filters_json("{}").is_err());
        assert!(parse_filters_json("[{\"type\":\"bogus\"}]").is_err());
        assert!(parse_filters_json("not json").is_err());
    }

    #[test]
    fn null_optional_fields_are_absent() {
        // `decodeIfPresent` accepts explicit nulls for optional keys.
        let filters = parse_filters_json(
            r#"[
                {"id":"s","type":"sort","options":["a"],"default":null,"canAscend":null},
                {"id":"c","type":"check","default":null,"canExclude":null},
                {"id":"sel","type":"select","options":["a"],"default":null,"ids":null}
            ]"#,
        )
        .unwrap();
        match &filters[0].config {
            FilterTypeConfig::Sort {
                can_ascend,
                default_value,
                ..
            } => {
                assert!(*can_ascend);
                assert!(default_value.is_none());
            }
            o => panic!("{o:?}"),
        }
        match &filters[1].config {
            FilterTypeConfig::Check {
                can_exclude,
                default_value,
                ..
            } => {
                assert!(!can_exclude);
                assert!(default_value.is_none());
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn rejects_malformed_fields_instead_of_dropping() {
        // A non-string option element must not be silently filtered out —
        // that would misalign `options` with `ids`.
        assert!(parse_filters_json(r#"[{"type":"select","options":["a",1]}]"#).is_err());
        assert!(
            parse_filters_json(r#"[{"type":"select","options":[] , "ids":["a",null]}]"#).is_err()
        );
        assert!(parse_filters_json(r#"[{"type":"check","canExclude":"yes"}]"#).is_err());
        assert!(parse_filters_json(r#"[{"type":"range","min":"0"}]"#).is_err());
        assert!(parse_filters_json(r#"[{"type":"select","options":null,"ids":{"x":1}}]"#).is_err());
        // sort default.index must be an integer
        assert!(
            parse_filters_json(r#"[{"type":"sort","options":["a"],"default":{"index":"0"}}]"#)
                .is_err()
        );
    }

    #[test]
    fn resolved_defaults_cover_each_kind() {
        let filters = parse_filters_json(
            r#"[
                {"id":"t","type":"text","placeholder":"x"},
                {"id":"order","type":"sort","options":["a","b"],
                 "default":{"index":1,"ascending":true}},
                {"id":"nosort","type":"sort","options":["a","b"]},
                {"id":"adult","type":"check","default":true},
                {"id":"excl","type":"check","default":false},
                {"id":"unset","type":"check"},
                {"id":"status","type":"select","options":["x","y"],"default":"y"},
                {"id":"noopt","type":"select","options":["first","second"],
                 "ids":["f","s"]},
                {"id":"genre","type":"multi-select","options":["g1","g2"],
                 "defaultIncluded":["g1"],"defaultExcluded":["g2"]},
                {"id":"empty","type":"multi-select","options":["g1"]},
                {"id":"note","type":"note","text":"n"},
                {"id":"r","type":"range","min":0,"max":10}
            ]"#,
        )
        .unwrap();
        let values = default_filter_values(&filters);
        assert_eq!(
            values,
            vec![
                FilterValue::Text {
                    id: "t".into(),
                    value: "".into()
                },
                FilterValue::Sort(SortFilterValue {
                    id: "order".into(),
                    index: 1,
                    ascending: true
                }),
                // implicit sort default: index 0, ascending false
                FilterValue::Sort(SortFilterValue {
                    id: "nosort".into(),
                    index: 0,
                    ascending: false
                }),
                // check: true -> 1 (include)
                FilterValue::Check {
                    id: "adult".into(),
                    value: 1
                },
                // check: false -> 2 (exclude) — NOT 0
                FilterValue::Check {
                    id: "excl".into(),
                    value: 2
                },
                // check: absent -> 0 (neutral)
                FilterValue::Check {
                    id: "unset".into(),
                    value: 0
                },
                FilterValue::Select {
                    id: "status".into(),
                    value: "y".into()
                },
                // resolved select default: ids.first when no explicit default
                FilterValue::Select {
                    id: "noopt".into(),
                    value: "f".into()
                },
                FilterValue::MultiSelect {
                    id: "genre".into(),
                    included: vec!["g1".into()],
                    excluded: vec!["g2".into()]
                },
                // empty multiselect baseline
                FilterValue::MultiSelect {
                    id: "empty".into(),
                    included: vec![],
                    excluded: vec![]
                },
                FilterValue::Range {
                    id: "r".into(),
                    from: None,
                    to: None
                },
            ]
        );
    }

    #[test]
    fn filter_value_serde_uses_upstream_flat_shape() {
        let value = FilterValue::Check {
            id: "adult".into(),
            value: 2,
        };
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, r#"{"type":2,"id":"adult","value":2}"#);
        let back: FilterValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, value);

        let sort = FilterValue::Sort(SortFilterValue {
            id: "order".into(),
            index: 1,
            ascending: true,
        });
        let json = serde_json::to_string(&sort).unwrap();
        assert_eq!(
            json,
            r#"{"type":1,"id":"order","index":1,"ascending":true}"#
        );
        assert_eq!(serde_json::from_str::<FilterValue>(&json).unwrap(), sort);

        let multi: FilterValue =
            serde_json::from_str(r#"{"type":4,"id":"genre","included":["a"],"excluded":[]}"#)
                .unwrap();
        assert_eq!(
            multi,
            FilterValue::MultiSelect {
                id: "genre".into(),
                included: vec!["a".into()],
                excluded: vec![]
            }
        );

        let range: FilterValue =
            serde_json::from_str(r#"{"type":5,"id":"year","from":2000,"to":null}"#).unwrap();
        assert_eq!(
            range,
            FilterValue::Range {
                id: "year".into(),
                from: Some(2000.0),
                to: None
            }
        );
    }
}
