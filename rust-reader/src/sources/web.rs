use crate::design;






use egui::*;
use serde::Deserialize;
use serde::Serialize;





use std::path::Path;











use yejian_sources::*;
use super::*;
use super::engine::Task;
use super::ui_library::language_label;
use super::ui_browse::quiet_action;

/// What one verification/login attempt is for and how it presents.
pub(super) struct WebSession {
    /// Source the session belongs to (engine reload/login target).
    pub(super) source: String,
    /// `settings.json` login key, or `None` for a Cloudflare verification.
    pub(super) login: Option<String>,
    /// The URL the user should verify against.
    pub(super) url: String,
    /// Pasted cookie text, kept across frames while the dialog is open.
    pub(super) paste: String,
    /// Optional browser User-Agent for the site (Cloudflare UA pinning).
    pub(super) agent: String,
}

/// One site's saved web cookies plus the browser UA pinned with them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct WebSiteCookies {
    pub(super) cookies: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) user_agent: Option<String>,
}

pub(super) const WEB_COOKIES_FILE: &str = "web-cookies.json";

/// Unknown shapes degrade to an empty site instead of dropping the file.
pub(super) fn load_web_cookies(root: &Path) -> std::collections::BTreeMap<String, WebSiteCookies> {
    let Ok(bytes) = std::fs::read(root.join(WEB_COOKIES_FILE)) else {
        return Default::default();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Default::default();
    };
    let Some(map) = value.as_object() else {
        return Default::default();
    };
    let mut sites = std::collections::BTreeMap::new();
    for (url, entry) in map {
        let pairs = |value: &serde_json::Value| -> Vec<(String, String)> {
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|pair| {
                            Some((
                                pair.get(0)?.as_str()?.to_string(),
                                pair.get(1)?.as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let site = if entry.is_array() {
            // Legacy format: bare cookie pair list.
            WebSiteCookies {
                cookies: pairs(entry),
                user_agent: None,
            }
        } else {
            WebSiteCookies {
                cookies: entry.get("cookies").map(&pairs).unwrap_or_default(),
                user_agent: entry
                    .get("user_agent")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }
        };
        if !site.cookies.is_empty() || site.user_agent.is_some() {
            sites.insert(url.clone(), site);
        }
    }
    sites
}

pub(super) type SettingsStore =
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, SettingValue>>;

pub(super) const SETTINGS_FILE: &str = "source-settings.json";

pub(super) fn load_source_settings(root: &Path) -> SettingsStore {
    std::fs::read(root.join(SETTINGS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn save_source_settings(root: &Path, store: &SettingsStore) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let part = root.join(format!("{SETTINGS_FILE}.part"));
    std::fs::write(&part, serde_json::to_vec_pretty(store)?)?;
    std::fs::rename(&part, root.join(SETTINGS_FILE))?;
    Ok(())
}

pub(super) fn setting_row(
    ui: &mut Ui,
    item: &SettingItem,
    current: Option<SettingValue>,
) -> Option<SettingValue> {
    let mut changed = None;
    let label = |ui: &mut Ui| {
        ui.add(Label::new(RichText::new(&item.title).strong()).wrap());
    };
    match &item.kind {
        SettingKind::Login { .. } => {}
        SettingKind::Button { .. } | SettingKind::Link { .. } => {}
        SettingKind::Toggle => {
            let mut on = matches!(current, Some(SettingValue::Bool(true)));
            if ui.add(Checkbox::new(&mut on, &item.title)).changed() {
                changed = Some(SettingValue::Bool(on));
            }
        }
        SettingKind::Select { values, titles } => {
            label(ui);
            let selected = match &current {
                Some(SettingValue::Str(value)) => Some(value.as_str()),
                _ => None,
            };
            ui.horizontal_wrapped(|ui| {
                for (value, title) in values.iter().zip(titles) {
                    if ui
                        .selectable_label(selected == Some(value.as_str()), title)
                        .clicked()
                    {
                        changed = Some(SettingValue::Str(value.clone()));
                    }
                }
            });
        }
        SettingKind::Segment { titles } => {
            label(ui);
            let selected = match current {
                Some(SettingValue::Int(index)) => index,
                Some(SettingValue::Float(index)) => index as i32,
                _ => -1,
            };
            ui.horizontal_wrapped(|ui| {
                for (index, title) in titles.iter().enumerate() {
                    if ui
                        .selectable_label(selected == index as i32, title)
                        .clicked()
                    {
                        changed = Some(SettingValue::Int(index as i32));
                    }
                }
            });
        }
        SettingKind::MultiSelect { values, titles } => {
            label(ui);
            let mut chosen = match current {
                Some(SettingValue::StrList(list)) => list,
                _ => Vec::new(),
            };
            ui.horizontal_wrapped(|ui| {
                for (value, title) in values.iter().zip(titles) {
                    let on = chosen.contains(value);
                    let shown = if item.key == "languages" {
                        language_label(title).to_string()
                    } else {
                        title.clone()
                    };
                    if ui.selectable_label(on, shown).clicked() {
                        if on {
                            chosen.retain(|v| v != value);
                        } else {
                            chosen.push(value.clone());
                        }
                        changed = Some(SettingValue::StrList(chosen.clone()));
                    }
                }
            });
        }
        SettingKind::Text { placeholder } => {
            label(ui);
            let mut text = match current {
                Some(SettingValue::Str(text)) => text,
                _ => String::new(),
            };
            let edit = TextEdit::singleline(&mut text)
                .hint_text(placeholder.as_str())
                .desired_width(ui.available_width());
            if ui.add(edit).changed() {
                changed = Some(SettingValue::Str(text));
            }
        }
        SettingKind::Stepper { min, max, step } => {
            let value = match current {
                Some(SettingValue::Int(v)) => v as f64,
                Some(SettingValue::Float(v)) => v as f64,
                _ => *min,
            };
            let integral = step.fract() == 0.0 && min.fract() == 0.0;
            ui.horizontal_wrapped(|ui| {
                label(ui);
                let mut next = None;
                if ui
                    .add_enabled(value > *min, Button::new("−").min_size(vec2(40., 36.)))
                    .clicked()
                {
                    next = Some((value - step).max(*min));
                }
                ui.label(if integral {
                    format!("{}", value as i64)
                } else {
                    format!("{value:.2}")
                });
                if ui
                    .add_enabled(value < *max, Button::new("+").min_size(vec2(40., 36.)))
                    .clicked()
                {
                    next = Some((value + step).min(*max));
                }
                changed = next.map(|v| {
                    if integral {
                        SettingValue::Int(v as i32)
                    } else {
                        SettingValue::Float(v as f32)
                    }
                });
            });
        }
    }
    changed
}

pub(super) fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.split(':').next().unwrap_or("").to_ascii_lowercase()
}

impl Sources {
    pub(super)fn current_settings(&self) -> Vec<SettingItem> {
        self.installed
            .iter()
            .find(|source| source.manifest.id == self.selected_source)
            .map(|source| source_settings(&source.manifest, source.settings_json().as_deref()))
            .unwrap_or_default()
    }
    pub(super)fn change_setting(&mut self, key: &str, value: SettingValue) {
        let source = self.selected_source.clone();
        set_setting(&source, key, value.clone());
        self.source_settings
            .entry(source.clone())
            .or_default()
            .insert(key.to_string(), value);
        self.stale_engines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(source);
        self.settings_changed = true;
        if let Err(error) = save_source_settings(&self.root, &self.source_settings) {
            self.status = format!("书源设置保存失败：{error:#}");
        }
    }
    pub(super)fn open_web(&mut self, url: &str, login: Option<String>) {
        if yejian_platform::has_in_app_web() {
            match yejian_platform::open_web(url, DEFAULT_USER_AGENT) {
                Ok(()) => {
                    self.web_session = Some(WebSession {
                        source: self.selected_source.clone(),
                        login,
                        url: url.to_string(),
                        paste: String::new(),
                        agent: String::new(),
                    });
                }
                Err(error) => self.status = format!("无法打开网页：{error}"),
            }
            return;
        }
        // Desktop: hand the page to the system browser; the modal collects

        // the cookies the user copies back.
        match yejian_platform::open_external_url(url) {
            Ok(()) => {
                self.web_session = Some(WebSession {
                    source: self.selected_source.clone(),
                    login,
                    url: url.to_string(),
                    paste: String::new(),
                    agent: String::new(),
                });
                self.status = "已在浏览器打开，请完成验证后回到这里".into();
            }
            Err(error) => self.status = format!("无法打开浏览器：{error}"),
        }
    }
    /// else opens inside the app. Shell string invocation is never used.
    pub(super)fn open_link(&mut self, url: &str, external: bool) {
        if external || !yejian_platform::has_in_app_web() {
            match yejian_platform::open_external_url(url) {
                Ok(()) => {}
                Err(error) => self.status = format!("无法打开链接：{error}"),
            }
            return;
        }
        match yejian_platform::open_web(url, DEFAULT_USER_AGENT) {
            // Plain links don't feed cookies back into the verification flow,

            // so `web_session` stays empty and `finish_web` ignores the page.
            Ok(()) => {}
            Err(error) => self.status = format!("无法打开链接：{error}"),
        }
    }
    pub(super)fn finish_web(&mut self, result: yejian_platform::WebResult) {
        let Some(session) = self.web_session.take() else {
            return;
        };
        let Some(cookies) = result.cookies else {
            self.status = "已取消网页验证".into();
            return;
        };
        self.commit_web_session(session.source, session.login, &result.url, cookies, None);
    }
    /// login or reload it.
    pub(super)fn commit_web_session(
        &mut self,
        source: String,
        login: Option<String>,
        url: &str,
        cookies: Vec<(String, String)>,
        user_agent: Option<String>,
    ) {
        add_web_cookies(url, &cookies);
        if let Some(agent) = user_agent.as_ref().filter(|agent| !agent.trim().is_empty()) {
            set_site_user_agent(url, agent.trim());
        }
        let site = self.web_cookies.entry(url.to_string()).or_default();
        site.cookies = cookies.clone();
        if let Some(agent) = user_agent.filter(|agent| !agent.trim().is_empty()) {
            site.user_agent = Some(agent.trim().to_string());
        }
        if let Err(error) = self.save_web_cookies() {
            self.status = format!("网页验证结果保存失败：{error:#}");
        }
        if let Some(key) = login {
            self.submit(Task::Login {
                source,
                key,
                credentials: Err(cookies),
            });
            return;
        }
        self.challenge = None;
        self.status = "网页验证完成，正在重新加载".into();
        if self.selected_source == source && !source.is_empty() {
            self.select_source("");
            self.select_source(&source);
        }
    }
    /// in-app browser; every control meets the 44pt touch target.
    pub(crate) fn web_dialog(&mut self, ctx: &egui::Context) {
        if yejian_platform::has_in_app_web() {
            return;
        }
        let Some(session) = self.web_session.as_mut() else {
            return;
        };
        // Editable fields move out for the frame so the modal can mutate

        // `self` freely (status line, commit on finish).
        let url = session.url.clone();
        let mut paste = std::mem::take(&mut session.paste);
        let mut agent = std::mem::take(&mut session.agent);
        let mut finish = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("web-verify-dialog")).show(ctx, |ui| { ui.set_width(ui.available_width().min(460.).max(280.)); design::section(ui, "网页验证与登录"); ui.add( Label::new( RichText::new(&url) .color(design::palette(ui.visuals().dark_mode).muted) .small(), ) .wrap(), ); ui.add_space(8.); ui.horizontal_wrapped(|ui| { if ui.add(quiet_action("重新在浏览器打开")).clicked() && let Err(error) = yejian_platform::open_external_url(&url) { self.status = format!("无法打开浏览器：{error}"); } if ui.add(quiet_action("复制默认 UA")).clicked() { ui.ctx().copy_text(DEFAULT_USER_AGENT.into()); } }); ui.add_space(8.); ui.label("把浏览器里的 Cookie 粘贴到这里（支持 name=value 串、Cookie: 整行、cookies.txt 或 JSON）："); ui.add( TextEdit::multiline(&mut paste) .desired_width(ui.available_width()) .desired_rows(4), ); ui.add_space(6.); ui.label( RichText::new( "浏览器 User-Agent（Cloudflare 验证需与浏览器一致，留空保持默认）：", ) .small(), ); ui.add( TextEdit::singleline(&mut agent) .desired_width(ui.available_width()), ); ui.add_space(10.); ui.horizontal_wrapped(|ui| { let ready = !crate::trackers::parse_cookie_paste(&paste).is_empty(); if ui .add_enabled(ready, design::primary("完成导入", ui.visuals().dark_mode)) .clicked() { finish = true; } if ui.add(quiet_action("取消")).clicked() { cancel = true; } }); if finish { } });
        if finish {
            let Some(session) = self.web_session.take() else {
                return;
            };
            let cookies = crate::trackers::parse_cookie_paste(&paste);
            let agent = (!agent.trim().is_empty()).then_some(agent);
            self.commit_web_session(session.source, session.login, &session.url, cookies, agent);
        } else if cancel {
            self.web_session = None;
            self.status = "已取消网页验证".into();
        } else if let Some(session) = self.web_session.as_mut() {
            session.paste = paste;
            session.agent = agent;
        }
    }
    pub(super)fn forget_source_cookies(&mut self) {
        let urls: Vec<String> = self
            .installed
            .iter()
            .find(|source| source.manifest.id == self.selected_source)
            .map(|source| source.manifest.urls.clone())
            .unwrap_or_default();
        let hosts: Vec<String> = urls.iter().map(|url| url_host(url)).collect();
        let matches = |host: &str| {
            hosts.iter().any(|known| {
                host == known
                    || host.ends_with(&format!(".{known}"))
                    || known.ends_with(&format!(".{host}"))
            })
        };
        let mut forget: Vec<String> = urls;
        forget.extend(
            self.web_cookies
                .keys()
                .filter(|url| matches(&url_host(url)))
                .cloned(),
        );
        for url in &forget {
            clear_site_cookies(url);
        }
        self.web_cookies.retain(|url, _| !matches(&url_host(url)));
        if let Err(error) = self.save_web_cookies() {
            self.status = format!("网页登录信息清除失败：{error:#}");
        }
    }
    pub(super)fn save_web_cookies(&self) -> anyhow::Result<()> {
        let part = self.root.join(format!("{WEB_COOKIES_FILE}.part"));
        std::fs::write(&part, serde_json::to_vec_pretty(&self.web_cookies)?)?;
        std::fs::rename(&part, self.root.join(WEB_COOKIES_FILE))?;
        Ok(())
    }
    pub(super)fn login_row(&mut self, ui: &mut Ui, item: &SettingItem, web: bool, url: Option<&str>) {
        let source = self.selected_source.clone();
        let marker = format!("{}.username", item.key);
        let user = match setting_value(&source, &marker) {
            Some(SettingValue::Str(user)) if !user.is_empty() => Some(user),
            _ => None,
        };
        ui.add(Label::new(RichText::new(&item.title).strong()).wrap());
        if let Some(user) = user {
            ui.label(format!("已登录：{user}"));
            if ui.add(quiet_action("退出登录")).clicked() {
                self.change_setting(&marker, SettingValue::Str(String::new()));
                self.forget_source_cookies();
            }
            return;
        }
        if web {
            if ui
                .add_enabled(!self.busy, quiet_action("打开网页登录"))
                .clicked()
            {
                if let Some(url) = url {
                    self.open_web(url, Some(item.key.clone()));
                }
            }
            return;
        }
        ui.add(
            TextEdit::singleline(&mut self.login_user)
                .hint_text("账号")
                .desired_width(ui.available_width()),
        );
        ui.add(
            TextEdit::singleline(&mut self.login_pass)
                .password(true)
                .hint_text("密码")
                .desired_width(ui.available_width()),
        );
        let ready = !self.busy && !self.login_user.is_empty() && !self.login_pass.is_empty();
        if ui
            .add_enabled(ready, design::primary("登录", ui.visuals().dark_mode))
            .clicked()
        {
            self.submit(Task::Login {
                source,
                key: item.key.clone(),
                credentials: Ok((self.login_user.clone(), self.login_pass.clone())),
            });
        }
    }
    pub(super)fn close_settings(&mut self) {
        self.settings_open = false;
        if std::mem::take(&mut self.settings_changed) {
            // Reloads listings and results with the restarted engine.

            let source = self.selected_source.clone();
            self.select_source("");
            self.select_source(&source);
        }
    }
    pub(super)fn settings_page(&mut self, ui: &mut Ui) {
        if ui.add(quiet_action("‹ 返回书源")).clicked() {
            self.close_settings();
            return;
        }
        let items = self.current_settings();
        let source = self.selected_source.clone();
        ScrollArea::vertical()
            .id_salt(("source-settings", &source))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width());
                design::section(ui, "书源设置");
                ui.label(RichText::new("修改后返回书源即生效。").small());
                let status_visible = self.status_fresh(ui.ctx());
                if self.busy || status_visible {
                    ui.add(
                        Label::new(
                            RichText::new(&self.status)
                                .color(design::palette(ui.visuals().dark_mode).muted),
                        )
                        .wrap(),
                    );
                }
                let mut group = String::new();
                for item in items {
                    if item.group != group {
                        group = item.group.clone();
                        if !group.is_empty() {
                            design::section(ui, &group);
                        }
                    }
                    ui.add_space(6.);
                    if let SettingKind::Login { web, url } = &item.kind {
                        self.login_row(ui, &item, *web, url.as_deref());
                        continue;
                    }
                    match &item.kind {
                        SettingKind::Button {
                            notification,
                            destructive,
                            refreshes: _,
                        } => {
                            let palette = design::palette(ui.visuals().dark_mode);
                            let label = RichText::new(&item.title).color(if *destructive {
                                palette.danger
                            } else {
                                palette.ink
                            });
                            let button = Button::new(label)
                                .min_size(vec2(ui.available_width().min(220.), 44.))
                                .wrap();
                            if ui.add_enabled(!self.busy, button).clicked() {
                                let note = notification
                                    .clone()
                                    .filter(|n| !n.is_empty())
                                    .unwrap_or_else(|| item.key.clone());
                                if note.is_empty() {
                                    self.status = "此按钮没有可发送的通知".into();
                                } else {
                                    self.submit(Task::Notify {
                                        source: source.clone(),
                                        notification: note,
                                    });
                                    self.status = format!("已发送：{}", item.title);
                                }
                            }
                            continue;
                        }
                        SettingKind::Link { url, external } => {
                            let palette = design::palette(ui.visuals().dark_mode);
                            ui.horizontal_wrapped(|ui| {
                                ui.add(Label::new(RichText::new(&item.title).strong()).wrap());
                                let button = Button::new(
                                    RichText::new(url).color(palette.accent).underline(),
                                )
                                .frame_when_inactive(false)
                                .min_size(vec2(0., 44.))
                                .wrap();
                                if ui.add(button).clicked() {
                                    self.open_link(url, *external);
                                }
                            });
                            continue;
                        }
                        _ => {}
                    }
                    let current = setting_value(&source, &item.key).or(item.default.clone());
                    if let Some(value) = setting_row(ui, &item, current) {
                        self.change_setting(&item.key, value);
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn source_settings_survive_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::default();
        store
            .entry("demo.source".into())
            .or_default()
            .insert("quality".into(), SettingValue::Str("hd".into()));
        save_source_settings(root.path(), &store).unwrap();
        assert_eq!(load_source_settings(root.path()), store);
    }
}
