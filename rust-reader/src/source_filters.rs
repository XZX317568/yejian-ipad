//! Source-defined filters. Drafts are independent from the submitted search:
//! cancelling cannot change the results, and invalid ranges cannot be applied.
use crate::design;
use egui::*;
use std::collections::HashMap;
use yejian_sources::{
    Filter, FilterTypeConfig, FilterValue, SortFilterValue, default_filter_values,
};

pub(crate) fn value_id(value: &FilterValue) -> &str {
    match value {
        FilterValue::Text { id, .. }
        | FilterValue::Check { id, .. }
        | FilterValue::Select { id, .. }
        | FilterValue::MultiSelect { id, .. }
        | FilterValue::Range { id, .. } => id,
        FilterValue::Sort(value) => &value.id,
    }
}

fn option_id<'a>(ids: &'a Option<Vec<String>>, options: &'a [String], index: usize) -> &'a str {
    ids.as_ref()
        .and_then(|ids| ids.get(index))
        .unwrap_or(&options[index])
}

pub(crate) fn effective_values(schema: &[Filter], overrides: &[FilterValue]) -> Vec<FilterValue> {
    let mut values = default_filter_values(schema);
    for value in overrides {
        if !schema
            .iter()
            .any(|filter| filter.id == value_id(value) && valid_value(filter, value))
        {
            continue;
        }
        values.retain(|old| value_id(old) != value_id(value));
        values.push(value.clone());
    }
    values
}

fn valid_value(filter: &Filter, value: &FilterValue) -> bool {
    match (&filter.config, value) {
        (FilterTypeConfig::Text { .. }, FilterValue::Text { .. }) => true,
        (
            FilterTypeConfig::Sort {
                options,
                can_ascend,
                ..
            },
            FilterValue::Sort(value),
        ) => {
            value.index >= 0
                && (value.index as usize) < options.len()
                && (*can_ascend || !value.ascending)
        }
        (
            FilterTypeConfig::Check {
                can_exclude,
                default_value,
                ..
            },
            FilterValue::Check { value, .. },
        ) => {
            *value == 0
                || *value == 1
                || (*value == 2 && (*can_exclude || *default_value == Some(false)))
        }
        (FilterTypeConfig::Select(config), FilterValue::Select { value, .. }) => config
            .options
            .iter()
            .enumerate()
            .any(|(i, _)| option_id(&config.ids, &config.options, i) == value),
        (
            FilterTypeConfig::MultiSelect(config),
            FilterValue::MultiSelect {
                included, excluded, ..
            },
        ) => {
            (config.can_exclude || excluded.is_empty())
                && included.iter().chain(excluded).all(|id| {
                    config
                        .options
                        .iter()
                        .enumerate()
                        .any(|(i, _)| option_id(&config.ids, &config.options, i) == id)
                })
                && !included.iter().any(|id| excluded.contains(id))
        }
        (FilterTypeConfig::Range { min, max, decimal }, FilterValue::Range { from, to, .. }) => {
            parse_range(
                &filter.id,
                &from.map(|v| v.to_string()).unwrap_or_default(),
                &to.map(|v| v.to_string()).unwrap_or_default(),
                *min,
                *max,
                *decimal,
            )
            .is_ok()
        }
        _ => false,
    }
}

/// Only changes from the source's defaults go over the wire, like Aidoku.
/// Explicit neutral/empty values are retained when overriding a nonempty default.
pub(crate) fn overrides(schema: &[Filter], values: &[FilterValue]) -> Vec<FilterValue> {
    let defaults = default_filter_values(schema);
    schema
        .iter()
        .filter_map(|filter| {
            let value = values.iter().find(|value| value_id(value) == filter.id)?;
            let default = defaults.iter().find(|value| value_id(value) == filter.id);
            if default == Some(value) {
                return None;
            }
            let empty = match value {
                FilterValue::Text { value, .. } => value.is_empty(),
                FilterValue::Check { value, .. } => *value == 0,
                FilterValue::Select { value, .. } => value.is_empty(),
                FilterValue::MultiSelect {
                    included, excluded, ..
                } => included.is_empty() && excluded.is_empty(),
                FilterValue::Range { from, to, .. } => from.is_none() && to.is_none(),
                FilterValue::Sort(_) => false,
            };
            if default.is_none() && empty {
                None
            } else {
                Some(value.clone())
            }
        })
        .collect()
}

#[derive(Default)]
pub(crate) struct FilterEditor {
    pub values: Vec<FilterValue>,
    ranges: HashMap<String, (String, String)>,
    option_search: HashMap<String, String>,
}

fn choice<'a>(ui: &Ui, label: &'a str, selected: bool) -> Button<'a> {
    let palette = design::palette(ui.visuals().dark_mode);
    Button::new(RichText::new(label).color(if selected {
        palette.accent
    } else {
        palette.ink
    }))
    .selected(selected)
    .fill(if selected {
        palette.accent_soft
    } else {
        Color32::TRANSPARENT
    })
    .stroke(Stroke::new(
        1.,
        if selected {
            palette.accent
        } else {
            palette.line
        },
    ))
    .corner_radius(8)
    .min_size(vec2(44., 44.))
    .wrap()
}

fn option<'a>(
    ui: &Ui,
    label: &'a str,
    selected: bool,
    excluded: bool,
    tag_style: bool,
) -> Button<'a> {
    if tag_style {
        let mut button = choice(ui, label, selected);
        if excluded {
            button = button
                .fill(ui.visuals().error_fg_color.gamma_multiply(0.1))
                .stroke(Stroke::new(1., ui.visuals().error_fg_color));
        }
        return button;
    }
    let palette = design::palette(ui.visuals().dark_mode);
    let color = if excluded {
        ui.visuals().error_fg_color
    } else if selected {
        palette.accent
    } else {
        palette.ink
    };
    Button::new(RichText::new(label).color(color))
        .right_text(
            RichText::new(if selected { "●" } else { "○" }).color(if selected {
                color
            } else {
                palette.muted
            }),
        )
        .selected(selected)
        .frame_when_inactive(selected)
        .min_size(vec2(ui.available_width(), 44.))
        .wrap()
}

impl FilterEditor {
    pub fn new(schema: &[Filter], applied: &[FilterValue]) -> Self {
        let values = effective_values(schema, applied);
        let ranges = values
            .iter()
            .filter_map(|value| {
                if let FilterValue::Range { id, from, to } = value {
                    Some((
                        id.clone(),
                        (
                            from.map(|v| v.to_string()).unwrap_or_default(),
                            to.map(|v| v.to_string()).unwrap_or_default(),
                        ),
                    ))
                } else {
                    None
                }
            })
            .collect();
        Self {
            values,
            ranges,
            option_search: HashMap::new(),
        }
    }

    fn set(&mut self, value: FilterValue) {
        self.values.retain(|old| value_id(old) != value_id(&value));
        self.values.push(value);
    }

    pub fn submitted(&self, schema: &[Filter]) -> Result<Vec<FilterValue>, String> {
        let mut values = self.values.clone();
        for filter in schema {
            if let FilterTypeConfig::Range { min, max, decimal } = &filter.config {
                let (from, to) = self.ranges.get(&filter.id).cloned().unwrap_or_default();
                let value =
                    parse_range(&filter.id, &from, &to, *min, *max, *decimal).map_err(|error| {
                        format!("{}：{error}", filter.title.as_deref().unwrap_or(&filter.id))
                    })?;
                values.retain(|old| value_id(old) != filter.id);
                values.push(value);
            }
        }
        Ok(overrides(schema, &values))
    }

    pub fn ui(&mut self, ui: &mut Ui, schema: &[Filter]) {
        // Two readable columns on a wide iPad, one column in Split View or at
        // large text sizes. Each group keeps its controls together.
        let columns = if ui.available_width()
            >= 760. * (ui.text_style_height(&TextStyle::Body) / 20.).max(1.)
        {
            2
        } else {
            1
        };
        if columns == 1 {
            for filter in schema {
                self.group(ui, filter);
            }
        } else {
            let item_gap = ui.spacing().item_spacing.x;
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 32.;
                ui.columns(2, |uis| {
                    for column in uis.iter_mut() {
                        column.spacing_mut().item_spacing.x = item_gap;
                    }
                    for (index, filter) in schema.iter().enumerate() {
                        self.group(&mut uis[index % 2], filter);
                    }
                });
            });
        }
    }

    fn group(&mut self, ui: &mut Ui, filter: &Filter) {
        ui.push_id(&filter.id, |ui| {
            let palette = design::palette(ui.visuals().dark_mode);
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width());
                ui.set_min_width(ui.available_width());
                let current = self
                    .values
                    .iter()
                    .find(|value| value_id(value) == filter.id)
                    .cloned();
                let fallback = match filter.config {
                    FilterTypeConfig::Text { .. } => "关键词",
                    FilterTypeConfig::Sort { .. } => "排序",
                    FilterTypeConfig::Check { .. } => "条件",
                    FilterTypeConfig::Select(_) => "选项",
                    FilterTypeConfig::MultiSelect(_) => "标签",
                    FilterTypeConfig::Range { .. } => "范围",
                    FilterTypeConfig::Note(_) => "",
                };
                let title = filter.title.as_deref().unwrap_or(fallback);
                if !title.is_empty() {
                    ui.add(Label::new(RichText::new(title).strong().color(palette.ink)).wrap());
                    ui.add_space(8.);
                }
                match &filter.config {
                    FilterTypeConfig::Text { placeholder } => {
                        let mut value = match current {
                            Some(FilterValue::Text { value, .. }) => value,
                            _ => String::new(),
                        };
                        if ui
                            .add(
                                TextEdit::singleline(&mut value)
                                    .hint_text(placeholder.as_deref().unwrap_or("输入关键词"))
                                    .desired_width(ui.available_width()),
                            )
                            .changed()
                        {
                            self.set(FilterValue::Text {
                                id: filter.id.clone(),
                                value,
                            });
                        }
                    }
                    FilterTypeConfig::Sort {
                        can_ascend,
                        options,
                        default_value,
                    } => {
                        let mut value = match current {
                            Some(FilterValue::Sort(value)) => value,
                            _ => SortFilterValue {
                                id: filter.id.clone(),
                                index: default_value.as_ref().map_or(0, |v| v.index as i32),
                                ascending: default_value.as_ref().is_some_and(|v| v.ascending),
                            },
                        };
                        ui.horizontal_wrapped(|ui| {
                            for (index, option) in options.iter().enumerate() {
                                if ui
                                    .add(choice(ui, option, value.index == index as i32))
                                    .clicked()
                                {
                                    value.index = index as i32;
                                }
                            }
                        });
                        if *can_ascend {
                            ui.horizontal_wrapped(|ui| {
                                if ui.add(choice(ui, "降序", !value.ascending)).clicked() {
                                    value.ascending = false;
                                }
                                if ui.add(choice(ui, "升序", value.ascending)).clicked() {
                                    value.ascending = true;
                                }
                            });
                        }
                        if !options.is_empty() {
                            self.set(FilterValue::Sort(value));
                        }
                    }
                    FilterTypeConfig::Check {
                        name,
                        can_exclude,
                        default_value,
                    } => {
                        if let Some(name) = name {
                            if Some(name) != filter.title.as_ref() {
                                ui.label(name);
                            }
                        }
                        let mut value = match current {
                            Some(FilterValue::Check { value, .. }) => value,
                            _ => default_value.map_or(0, |v| if v { 1 } else { 2 }),
                        };
                        ui.horizontal_wrapped(|ui| {
                            for (state, label) in [(0, "不限"), (1, "包含"), (2, "排除")] {
                                if state == 2 && !can_exclude && *default_value != Some(false) {
                                    continue;
                                }
                                if ui.add(choice(ui, label, value == state)).clicked() {
                                    value = state;
                                }
                            }
                        });
                        self.set(FilterValue::Check {
                            id: filter.id.clone(),
                            value,
                        });
                    }
                    FilterTypeConfig::Select(config) => {
                        let mut value = match current {
                            Some(FilterValue::Select { value, .. }) => value,
                            _ => String::new(),
                        };
                        let search = self.option_search.entry(filter.id.clone()).or_default();
                        if config.options.len() >= 15 {
                            ui.add(
                                TextEdit::singleline(search)
                                    .hint_text("查找选项")
                                    .desired_width(ui.available_width()),
                            );
                        }
                        let needle = search.to_lowercase();
                        let mut draw = |ui: &mut Ui| {
                            let mut shown = 0;
                            for (index, label) in config.options.iter().enumerate() {
                                if !label.to_lowercase().contains(&needle) {
                                    continue;
                                }
                                shown += 1;
                                let id = option_id(&config.ids, &config.options, index);
                                if ui
                                    .add(option(
                                        ui,
                                        label,
                                        value == id,
                                        false,
                                        config.uses_tag_style,
                                    ))
                                    .clicked()
                                {
                                    value = id.to_string();
                                }
                            }
                            if shown == 0 {
                                ui.label("没有匹配的选项");
                            }
                        };
                        if config.uses_tag_style {
                            ui.horizontal_wrapped(draw);
                        } else if config.options.len() >= 15 {
                            // Long option lists get a bounded scroll of their
                            // own; the bottom fade dissolves clipped rows into
                            // the paper instead of slicing them in half.
                            let output = ScrollArea::vertical()
                                .id_salt("filter-options")
                                .max_height(ui.text_style_height(&TextStyle::Body) * 13.2)
                                .show(ui, draw);
                            design::paint_scroll_fade(
                                ui.painter(),
                                output.inner_rect,
                                palette.paper,
                                false,
                                true,
                            );
                        } else {
                            draw(ui);
                        }
                        self.set(FilterValue::Select {
                            id: filter.id.clone(),
                            value,
                        });
                    }
                    FilterTypeConfig::MultiSelect(config) => {
                        let (mut included, mut excluded) = match current {
                            Some(FilterValue::MultiSelect {
                                included, excluded, ..
                            }) => (included, excluded),
                            _ => (Vec::new(), Vec::new()),
                        };
                        if config.can_exclude {
                            ui.add(
                                Label::new(
                                    RichText::new("点击切换：不限 → 包含 → 排除")
                                        .small()
                                        .color(palette.muted),
                                )
                                .wrap(),
                            );
                        }
                        let search = self.option_search.entry(filter.id.clone()).or_default();
                        if config.options.len() >= 15 {
                            ui.add(
                                TextEdit::singleline(search)
                                    .hint_text("查找选项")
                                    .desired_width(ui.available_width()),
                            );
                        }
                        let needle = search.to_lowercase();
                        let mut draw = |ui: &mut Ui| {
                            let mut shown = 0;
                            for (index, label) in config.options.iter().enumerate() {
                                if !label.to_lowercase().contains(&needle) {
                                    continue;
                                }
                                shown += 1;
                                let id = option_id(&config.ids, &config.options, index);
                                let state = if included.iter().any(|v| v == id) {
                                    1
                                } else if excluded.iter().any(|v| v == id) {
                                    2
                                } else {
                                    0
                                };
                                let label = match state {
                                    1 => format!("✓ {label}"),
                                    2 => format!("− {label}"),
                                    _ => label.clone(),
                                };
                                if ui
                                    .add(option(
                                        ui,
                                        &label,
                                        state != 0,
                                        state == 2,
                                        config.uses_tag_style,
                                    ))
                                    .clicked()
                                {
                                    cycle_option(
                                        &mut included,
                                        &mut excluded,
                                        id,
                                        config.can_exclude,
                                    );
                                }
                            }
                            if shown == 0 {
                                ui.label("没有匹配的选项");
                            }
                        };
                        if config.uses_tag_style {
                            ui.horizontal_wrapped(draw);
                        } else if config.options.len() >= 15 {
                            let output = ScrollArea::vertical()
                                .id_salt("filter-options")
                                .max_height(ui.text_style_height(&TextStyle::Body) * 13.2)
                                .show(ui, draw);
                            design::paint_scroll_fade(
                                ui.painter(),
                                output.inner_rect,
                                palette.paper,
                                false,
                                true,
                            );
                        } else {
                            draw(ui);
                        }
                        self.set(FilterValue::MultiSelect {
                            id: filter.id.clone(),
                            included,
                            excluded,
                        });
                    }
                    FilterTypeConfig::Range { min, max, decimal } => {
                        let (from, to) = self.ranges.entry(filter.id.clone()).or_default();
                        // Stacked inputs remain usable at 320 pt and 200% text.
                        ui.label("最小值");
                        ui.add(
                            TextEdit::singleline(from)
                                .hint_text("不限")
                                .desired_width(ui.available_width()),
                        );
                        ui.label("最大值");
                        ui.add(
                            TextEdit::singleline(to)
                                .hint_text("不限")
                                .desired_width(ui.available_width()),
                        );
                        if let Err(error) = parse_range(&filter.id, from, to, *min, *max, *decimal)
                        {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        } else if min.is_some() || max.is_some() {
                            ui.add(
                                Label::new(
                                    RichText::new(format!(
                                        "允许范围：{} ～ {}",
                                        min.map_or("不限".into(), |v| v.to_string()),
                                        max.map_or("不限".into(), |v| v.to_string())
                                    ))
                                    .small()
                                    .color(palette.muted),
                                )
                                .wrap(),
                            );
                        }
                    }
                    FilterTypeConfig::Note(text) => {
                        ui.add(Label::new(RichText::new(text).color(palette.muted)).wrap());
                    }
                }
            });
            ui.add_space(14.);
            if !matches!(&filter.config, FilterTypeConfig::Note(_)) {
                ui.separator();
                ui.add_space(14.);
            }
        });
    }
}

fn cycle_option(
    included: &mut Vec<String>,
    excluded: &mut Vec<String>,
    id: &str,
    can_exclude: bool,
) {
    if let Some(index) = included.iter().position(|value| value == id) {
        included.remove(index);
        if can_exclude && !excluded.iter().any(|value| value == id) {
            excluded.push(id.to_string());
        }
    } else if let Some(index) = excluded.iter().position(|value| value == id) {
        excluded.remove(index);
    } else {
        included.push(id.to_string());
    }
}

fn parse_range(
    id: &str,
    from: &str,
    to: &str,
    min: Option<f32>,
    max: Option<f32>,
    decimal: bool,
) -> Result<FilterValue, String> {
    let parse = |text: &str| -> Result<Option<f32>, String> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        let value: f32 = text
            .trim()
            .parse()
            .map_err(|_| "请输入有效数字".to_string())?;
        if !value.is_finite() {
            return Err("请输入有限数字".into());
        }
        if !decimal && value.fract() != 0. {
            return Err("此项需要整数".into());
        }
        if min.is_some_and(|min| value < min) || max.is_some_and(|max| value > max) {
            return Err("数值超出允许范围".into());
        }
        Ok(Some(value))
    };
    let from = parse(from)?;
    let to = parse(to)?;
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return Err("最小值不能大于最大值".into());
        }
    }
    Ok(FilterValue::Range {
        id: id.into(),
        from,
        to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_exclude_cycle_never_overlaps() {
        let (mut included, mut excluded) = (vec![], vec![]);
        cycle_option(&mut included, &mut excluded, "fantasy-id", true);
        assert_eq!(included, ["fantasy-id"]);
        cycle_option(&mut included, &mut excluded, "fantasy-id", true);
        assert!(included.is_empty());
        assert_eq!(excluded, ["fantasy-id"]);
        cycle_option(&mut included, &mut excluded, "fantasy-id", true);
        assert!(included.is_empty() && excluded.is_empty());
        cycle_option(&mut included, &mut excluded, "fantasy-id", false);
        cycle_option(&mut included, &mut excluded, "fantasy-id", false);
        assert!(included.is_empty() && excluded.is_empty());
    }

    #[test]
    fn range_rejects_nan_inverted_out_of_bounds_and_fractional_integers() {
        for (from, to) in [
            ("NaN", ""),
            ("inf", ""),
            ("10", "2"),
            ("-1", ""),
            ("", "101"),
            ("1.5", ""),
        ] {
            assert!(parse_range("year", from, to, Some(0.), Some(100.), false).is_err());
        }
        assert!(matches!(
            parse_range("score", "1.5", "", None, None, true).unwrap(),
            FilterValue::Range {
                from: Some(1.5),
                to: None,
                ..
            }
        ));
    }

    #[test]
    fn resetting_uses_defaults_and_clearing_default_is_an_explicit_override() {
        let schema = vec![Filter {
            id: "tags".into(),
            title: None,
            hide_from_header: None,
            config: FilterTypeConfig::MultiSelect(yejian_sources::MultiSelectFilterCfg {
                is_genre: true,
                can_exclude: true,
                uses_tag_style: true,
                options: vec!["冒险".into()],
                ids: Some(vec!["adventure".into()]),
                default_included: Some(vec!["adventure".into()]),
                default_excluded: None,
            }),
        }];
        let mut draft = FilterEditor::new(&schema, &[]);
        assert!(draft.submitted(&schema).unwrap().is_empty());
        draft.set(FilterValue::MultiSelect {
            id: "tags".into(),
            included: vec![],
            excluded: vec![],
        });
        assert_eq!(draft.submitted(&schema).unwrap().len(), 1);
        assert!(
            FilterEditor::new(&schema, &[])
                .submitted(&schema)
                .unwrap()
                .is_empty()
        );
    }
}
