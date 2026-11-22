use crate::{
    follows,
    motion::{self, ShelfMotion},
    shelf::{self, Plane},
    sources::{STREAM_PREFIX, StreamChapter},
    worker::{Command, Event, PAGE_NOT_READY, Worker},
};
use chrono::Datelike;
use egui::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::atomic::Ordering,
};
use yejian_core::{Book, ChapterLink, ChapterMetadata};
use super::*;
use super::shelf_page::*;
use super::reader_page::*;
use super::settings_page::*;
use super::history_page::*;
use super::dialogs::*;
use super::test_support::*;


impl ReaderApp {
    pub(super) fn managed_categories(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::from(["未分类", "在线追更"]);
        let mut list = Vec::new();
        for name in &self.user_categories {
            if seen.insert(name.as_str()) {
                list.push(name.clone());
            }
        }
        let mut derived: Vec<String> = self
            .books
            .iter()
            .map(|book| book.category.clone())
            .filter(|name| !name.is_empty() && !seen.contains(name.as_str()))
            .collect();
        derived.sort();
        derived.dedup();
        list.extend(derived);
        list
    }
    /// Adds a user category (deduped, trimmed) and persists the list.
    pub(super) fn add_category(&mut self, name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty()
            || matches!(name, "全部" | "未分类" | "在线追更")
            || self.managed_categories().iter().any(|item| item == name)
        {
            return None;
        }
        let name = name.to_string();
        self.user_categories.push(name.clone());
        self.save_categories();
        Some(name)
    }
    /// Opaque scrim + card shared by the manage and pick panels — same
    /// pattern as the storage delete confirmations.
    pub(super) fn category_panels_ui(&mut self, ctx: &Context) {
        if self.category_manage {
            let mut open = true;
            let categories = self.managed_categories();
            self.opaque_card::<()>(ctx, "category-manage", &mut open, |app, ui| {
                ui.heading("分类管理");
                ScrollArea::vertical().max_height(300.).show(ui, |ui| {
                    for (index, name) in categories.iter().enumerate() {
                        if app.category_renaming.as_deref() == Some(name.as_str()) {
                            ui.horizontal(|ui| {
                                ui.add(
                                    TextEdit::singleline(&mut app.category_rename_edit)
                                        .desired_width(ui.available_width() - 60.),
                                );
                                if ui.button("确定").clicked() {
                                    let new = app.category_rename_edit.trim().to_string();
                                    if !new.is_empty() && new != *name {
                                        app.send(Command::RenameCategory(
                                            name.clone(),
                                            new.clone(),
                                        ));
                                        if let Some(pos) =
                                            app.user_categories.iter().position(|x| x == name)
                                        {
                                            app.user_categories[pos] = new.clone();
                                        } else {
                                            app.user_categories.push(new.clone());
                                        }
                                        app.save_categories();
                                        if app.category == *name {
                                            app.category = new;
                                        }
                                    }
                                    app.category_renaming = None;
                                }
                            });
                            continue;
                        }
                        ui.horizontal(|ui| {
                            ui.add(Label::new(name).wrap());
                            let pos = app
                                .user_categories
                                .iter()
                                .position(|item| item == name);
                            let up_enabled = pos.is_some_and(|pos| pos > 0);
                            let down_enabled = pos
                                .is_some_and(|pos| pos + 1 < app.user_categories.len());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(Button::new("删除").min_size(vec2(52., 0.)))
                                        .clicked()
                                    {
                                        app.storage_confirm =
                                            Some(StorageConfirm::Category(name.clone()));
                                    }
                                    if ui
                                        .add(Button::new("重命名").min_size(vec2(52., 0.)))
                                        .clicked()
                                    {
                                        app.category_renaming = Some(name.clone());
                                        app.category_rename_edit = name.clone();
                                    }
                                    let down = ui.add_enabled(
                                        down_enabled,
                                        Button::new("下移").min_size(vec2(52., 0.)),
                                    );
                                    if down_enabled {
                                        app.hitboxes
                                            .insert(format!("category-down-{index}"), down.rect);
                                    }
                                    if down.clicked()
                                        && let Some(pos) = pos
                                    {
                                        app.user_categories.swap(pos, pos + 1);
                                        app.save_categories();
                                    }
                                    let up = ui.add_enabled(
                                        up_enabled,
                                        Button::new("上移").min_size(vec2(52., 0.)),
                                    );
                                    if up_enabled {
                                        app.hitboxes
                                            .insert(format!("category-up-{index}"), up.rect);
                                    }
                                    if up.clicked()
                                        && let Some(pos) = pos
                                    {
                                        app.user_categories.swap(pos, pos - 1);
                                        app.save_categories();
                                    }
                                },
                            );
                        });
                        ui.add_space(4.);
                    }
                    if categories.is_empty() {
                        ui.label(RichText::new("还没有自建分类").small());
                    }
                });
                ui.add_space(6.);
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        TextEdit::singleline(&mut app.category_new)
                            .hint_text("新分类名")
                            .desired_width(ui.available_width() - 60.),
                    );
                    app.hitboxes.insert("category-new".into(), edit.rect);
                    let add = ui.button("新建");
                    app.hitboxes.insert("category-add".into(), add.rect);
                    if (add.clicked()
                        || edit.lost_focus()
                            && ui.input(|input| input.key_pressed(Key::Enter)))
                        && app.add_category(&app.category_new.clone()).is_some()
                    {
                        app.category_new.clear();
                    }
                });
                ui.add_space(6.);
                if ui.button("关闭").clicked() {
                    app.category_manage = false;
                }
            });
            self.category_manage = open && self.category_manage;
        }
        if self.category_pick {
            let mut open = true;
            let mut choices = vec!["未分类".to_string()];
            choices.extend(self.managed_categories());
            let books = self.selected_books();
            self.opaque_card::<()>(ctx, "category-pick", &mut open, |app, ui| {
                ui.heading("选择分类");
                ScrollArea::vertical().max_height(300.).show(ui, |ui| {
                    for (index, name) in choices.iter().enumerate() {
                        let row = ui.add_sized(
                            [ui.available_width(), 40.],
                            Button::new(name.clone()),
                        );
                        app.hitboxes
                            .insert(format!("category-pick-{index}-{name}"), row.rect);
                        if row.clicked() {
                            app.send(Command::Category(books.clone(), name.clone()));
                            app.selected.clear();
                            app.category_pick = false;
                        }
                    }
                });
                ui.add_space(6.);
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        TextEdit::singleline(&mut app.category_new)
                            .hint_text("新分类名")
                            .desired_width(ui.available_width() - 90.),
                    );
                    let add = ui.add_enabled(
                        !app.category_new.trim().is_empty(),
                        Button::new("新建并加入"),
                    );
                    if add.clicked()
                        && let Some(name) = app.add_category(&app.category_new.clone())
                    {
                        app.send(Command::Category(books.clone(), name));
                        app.selected.clear();
                        app.category_new.clear();
                        app.category_pick = false;
                    }
                    let _ = edit;
                });
                ui.add_space(6.);
                if ui.button("取消").clicked() {
                    app.category_pick = false;
                }
            });
            self.category_pick = open && self.category_pick;
        }
    }
}
