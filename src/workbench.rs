//! 脚本 tab: write, run and debug a model's rescue scripts.
//!
//! The editor shows one profile field (解锁 `unlock` or 一键救援 `enable`)
//! with line numbers and live validation; scripts run through the rescue
//! worker as a whole (F5), one step at a time (F10) or just the line under
//! the cursor (Ctrl+Enter), and can be stopped (Esc). The gutter marks the
//! next step (▶) and the line executing now (●). Saving writes only that
//! profile's section of rescue-profiles.txt.

use crate::checkup;
use crate::guide;
use crate::profiles::{self, Profile};
use crate::rescue::{self, Located, Rescue, ScriptError};
use crate::theme;
use crate::usbid::UsbTarget;
use crate::widgets;
use eframe::egui::{self, Color32, Key, Margin, RichText, text::LayoutJob, vec2};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Unlock,
    Enable,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Unlock => "解锁",
            Field::Enable => "一键救援",
        }
    }

    fn get(self, p: &Profile) -> String {
        match self {
            Field::Unlock => p.unlock.clone(),
            Field::Enable => p.enable.clone().unwrap_or_default(),
        }
    }

    fn set(self, p: &mut Profile, script: String) {
        match self {
            Field::Unlock => p.unlock = script,
            Field::Enable => p.enable = (!script.trim().is_empty()).then_some(script),
        }
    }
}

/// What the app passes in each frame.
pub struct Context<'a> {
    pub rescue: &'a Rescue,
    pub status: &'a rescue::Status,
    pub profiles: &'a [Profile],
    /// The profile in effect (救援 tab's choice / automatic).
    pub current: Option<&'a Profile>,
    /// Lock-screen password for `{pw}` (never shown).
    pub password: Option<String>,
    /// adb model of the selected device, to prefill a new profile.
    pub adb_model: Option<&'a str>,
}

/// What the app must do for the workbench.
pub enum Action {
    /// Write this profile into rescue-profiles.txt and reload.
    Save(Profile),
    Log(String),
    OpenGuide,
}

struct NewProfile {
    name: String,
    models: String,
    usb: String,
    /// USB functions present now (from 设备体检), offered as chips.
    candidates: Vec<checkup::Row>,
}

/// A profile/field switch waiting for the user's decision on unsaved edits.
struct PendingSwitch {
    profile: String,
    field: Field,
}

pub struct Workbench {
    text: String,
    /// Text as loaded / last saved; differs → unsaved edits.
    saved: String,
    /// Profile name + field the text belongs to.
    source: Option<(String, Field)>,
    /// Index (into the parsed steps) of the next single step.
    next_step: usize,
    /// 1-based line of the text cursor, from the last frame.
    cursor_line: usize,
    new_profile: Option<NewProfile>,
    pending: Option<PendingSwitch>,
    /// Text from the guide, applied on the next frame.
    inserted: Option<String>,
}

impl Default for Workbench {
    fn default() -> Self {
        Workbench {
            text: String::new(),
            saved: String::new(),
            source: None,
            next_step: 0,
            cursor_line: 1,
            new_profile: None,
            pending: None,
            inserted: None,
        }
    }
}

/// Parse for validation: `{pw}` stands in as a dummy so a missing password
/// is not a syntax error.
fn validate(text: &str) -> Result<Vec<Located>, ScriptError> {
    rescue::parse_located(text, &[("pw", "0")])
}

/// Lines the next step / the running step are on.
fn marker_lines(
    steps: &[Located],
    next_step: usize,
    running: Option<usize>,
) -> (Option<usize>, Option<usize>) {
    let next = running
        .is_none()
        .then(|| steps.get(next_step).map(|s| s.line))
        .flatten();
    (next, running)
}

/// 1-based line of a char offset.
fn line_of(text: &str, char_index: usize) -> usize {
    1 + text.chars().take(char_index).filter(|&c| c == '\n').count()
}

impl Workbench {
    /// Put text into the editor (e.g. from the guide), keeping the source.
    pub fn insert(&mut self, text: String) {
        self.inserted = Some(text);
    }

    pub fn mark_saved(&mut self, profile: &Profile) {
        if let Some((name, field)) = &self.source
            && *name == profile.name
        {
            self.saved = field.get(profile);
            self.text = self.saved.clone();
        }
    }

    fn load(&mut self, profile: &Profile, field: Field) {
        self.text = field.get(profile);
        self.saved = self.text.clone();
        self.source = Some((profile.name.clone(), field));
        self.next_step = 0;
        self.pending = None;
    }

    fn dirty(&self) -> bool {
        self.text != self.saved
    }

    fn request_switch(&mut self, profiles: &[Profile], name: &str, field: Field) {
        if self.dirty() {
            self.pending = Some(PendingSwitch {
                profile: name.to_string(),
                field,
            });
        } else if let Some(p) = profiles.iter().find(|p| p.name == name) {
            self.load(p, field);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &Context) -> Vec<Action> {
        let mut actions = Vec::new();
        // First show, or the profile vanished after a reload: follow the
        // profile in effect.
        let source_exists = self
            .source
            .as_ref()
            .is_some_and(|(name, _)| cx.profiles.iter().any(|p| &p.name == name));
        if !source_exists && let Some(p) = cx.current {
            let field = if p.enable.is_some() {
                Field::Enable
            } else {
                Field::Unlock
            };
            self.load(p, field);
        }
        // After the initial load, so an example inserted from the guide is
        // not overwritten; it shows as unsaved edits of the loaded field.
        if let Some(text) = self.inserted.take() {
            self.text = text;
            self.next_step = 0;
        }

        self.toolbar(ui, cx, &mut actions);
        if let Some(form) = &mut self.new_profile {
            if let Some(done) = new_profile_form(ui, form, cx.profiles) {
                if let Some(profile) = done {
                    actions.push(Action::Log(format!("✔ 新建机型「{}」", profile.name)));
                    self.load(&profile, Field::Enable);
                    actions.push(Action::Save(profile));
                }
                self.new_profile = None;
            }
            return actions;
        }
        self.pending_bar(ui, cx, &mut actions);

        let parsed = validate(&self.text);
        let steps = parsed.as_ref().map(Vec::as_slice).unwrap_or(&[]);
        if self.next_step >= steps.len() {
            self.next_step = 0;
        }
        self.run_bar(ui, cx, &parsed, &mut actions);

        let error_line = parsed.as_ref().err().map(|e| e.line);
        let (next, running) = marker_lines(
            steps,
            self.next_step,
            cx.status.line.filter(|_| cx.status.busy),
        );
        let footer_h = 30.0;
        let editor_h = (ui.available_height() - footer_h).max(120.0);
        self.editor(ui, editor_h, error_line, next, running);

        // Footer: validation result.
        let (text, color) = match &parsed {
            Ok(steps) if steps.is_empty() => ("脚本为空".to_string(), theme::c().text_faint),
            Ok(steps) => (format!("✔ 语法正确 · {} 步", steps.len()), theme::c().green),
            Err(e) => (format!("✘ {e}"), theme::c().red),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(text).font(theme::mono(11.0)).color(color));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::link(ui, "完整说明与命令参考 →").clicked() {
                    actions.push(Action::OpenGuide);
                }
            });
        });
        actions
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &Context, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("机型").color(theme::c().text_dim));
            let (name, field) = self
                .source
                .clone()
                .unwrap_or_else(|| (String::from("—"), Field::Enable));
            let mut chosen = name.clone();
            egui::ComboBox::from_id_salt("wb-profile")
                .width(200.0)
                .truncate()
                .selected_text(&name)
                .show_ui(ui, |ui| {
                    for p in cx.profiles {
                        ui.selectable_value(&mut chosen, p.name.clone(), &p.name);
                    }
                });
            if chosen != name {
                self.request_switch(cx.profiles, &chosen, field);
            }
            let mut f = field;
            // Fixed width: `segmented` fills what it is given, and in this
            // row that would push 保存 off into the rail.
            let changed = ui
                .allocate_ui(vec2(200.0, 28.0), |ui| {
                    widgets::segmented(
                        ui,
                        &mut f,
                        &[
                            (Field::Unlock, Field::Unlock.label()),
                            (Field::Enable, Field::Enable.label()),
                        ],
                    )
                })
                .inner;
            if changed && f != field {
                self.request_switch(cx.profiles, &name, f);
            }
            if widgets::link(ui, "新建机型").clicked() {
                self.new_profile = Some(NewProfile {
                    name: String::new(),
                    models: cx.adb_model.unwrap_or_default().to_string(),
                    usb: String::new(),
                    candidates: checkup::rows(&crate::usbhost::survey(), &[])
                        .into_iter()
                        .filter(|r| r.role != checkup::Role::Other || r.service.is_empty())
                        .collect(),
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_save = self.dirty() && validate(&self.text).is_ok();
                if ui
                    .add_enabled(can_save, egui::Button::new("保存到机型配置"))
                    .on_hover_text(profiles::path().display().to_string())
                    .clicked()
                    && let Some((name, field)) = &self.source
                    && let Some(p) = cx.profiles.iter().find(|p| &p.name == name)
                {
                    let mut p = p.clone();
                    field.set(&mut p, self.text.trim_end().to_string());
                    actions.push(Action::Save(p));
                }
                if self.dirty() {
                    if widgets::link(ui, "还原").clicked() {
                        self.text = self.saved.clone();
                    }
                    ui.label(RichText::new("● 未保存").color(theme::c().amber));
                }
            });
        });
    }

    fn pending_bar(&mut self, ui: &mut egui::Ui, cx: &Context, actions: &mut Vec<Action>) {
        let Some(pending) = &self.pending else {
            return;
        };
        let (profile, field) = (pending.profile.clone(), pending.field);
        egui::Frame::NONE
            .fill(theme::c().amber_lo)
            .corner_radius(theme::RADIUS)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("当前脚本有未保存的修改。").color(theme::c().text));
                    if widgets::link(ui, "保存后切换").clicked()
                        && let Some((name, cur)) = &self.source
                        && let Some(p) = cx.profiles.iter().find(|p| &p.name == name)
                    {
                        let mut p = p.clone();
                        cur.set(&mut p, self.text.trim_end().to_string());
                        actions.push(Action::Save(p));
                        self.saved = self.text.clone();
                        self.request_switch(cx.profiles, &profile, field);
                    }
                    if widgets::link(ui, "放弃修改").clicked() {
                        self.saved = self.text.clone(); // not dirty any more
                        self.request_switch(cx.profiles, &profile, field);
                    }
                    if widgets::link(ui, "取消").clicked() {
                        self.pending = None;
                    }
                });
            });
    }

    fn run_bar(
        &mut self,
        ui: &mut egui::Ui,
        cx: &Context,
        parsed: &Result<Vec<Located>, ScriptError>,
        actions: &mut Vec<Action>,
    ) {
        let busy = cx.status.busy;
        let ok = parsed.as_ref().is_ok_and(|s| !s.is_empty());
        let (f5, f10, esc, ctrl_enter) = ui.input(|i| {
            (
                i.key_pressed(Key::F5),
                i.key_pressed(Key::F10),
                i.key_pressed(Key::Escape),
                i.modifiers.command && i.key_pressed(Key::Enter),
            )
        });
        let line = self.cursor_line;
        let count = parsed.as_ref().map_or(0, Vec::len);
        let line_steps: Vec<usize> = parsed
            .as_ref()
            .map(|s| {
                s.iter()
                    .enumerate()
                    .filter(|(_, l)| l.line == line)
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        // What the user asked for this frame: (name, step indices, fresh).
        let mut request: Option<(&'static str, Vec<usize>, bool)> = None;
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let w = ((ui.available_width() - gap * 3.0) / 4.0).min(150.0);
            let size = vec2(w, 40.0);
            let can = ok && !busy;
            if widgets::key(ui, "▶ F5", "运行全部", size, can).clicked() || (f5 && can) {
                self.next_step = 0;
                request = Some(("脚本", (0..count).collect(), true));
            }
            if (widgets::key(ui, "⏭ F10", "单步", size, can).clicked() || (f10 && can)) && count > 0
            {
                let index = self.next_step.min(count - 1);
                self.next_step = (index + 1) % count;
                request = Some(("单步", vec![index], index == 0));
            }
            let can_line = !line_steps.is_empty() && !busy;
            if widgets::key(ui, "↳ Ctrl+Enter", "运行当前行", size, can_line)
                .on_hover_text(format!("第 {line} 行"))
                .clicked()
                || (ctrl_enter && can_line)
            {
                request = Some(("当前行", line_steps.clone(), false));
            }
            if widgets::key(ui, "■ Esc", "停止", size, busy).clicked() || (esc && busy) {
                cx.rescue.stop();
            }
        });
        if let Some((name, steps, fresh)) = request {
            self.send(cx, name, &steps, fresh, actions);
        }
        let state = if busy {
            match cx.status.line {
                Some(l) => format!("● 正在执行第 {l} 行"),
                None => "● 连接设备中…".into(),
            }
        } else {
            match parsed.as_ref().ok().and_then(|s| s.get(self.next_step)) {
                Some(step) => format!(
                    "▶ 下一步：第 {} 行 · 笔方向 rot:{}",
                    step.line,
                    // Step 1 starts a fresh run (default rotation).
                    if self.next_step == 0 {
                        rescue::DEFAULT_ROTATION
                    } else {
                        cx.status.rotation
                    }
                ),
                None => "写好脚本后可运行或单步调试".into(),
            }
        };
        let device = cx
            .status
            .device
            .clone()
            .unwrap_or_else(|| "设备未接管（运行时自动连接）".into());
        ui.label(
            RichText::new(format!("{state}    {device}"))
                .font(theme::mono(10.5))
                .color(theme::c().text_dim),
        );
    }

    /// Queue the steps at `indices` on the rescue worker, re-parsed with
    /// the real password (validation used a dummy `{pw}`; both parses give
    /// the same steps in the same order).
    fn send(
        &self,
        cx: &Context,
        name: &'static str,
        indices: &[usize],
        fresh: bool,
        actions: &mut Vec<Action>,
    ) {
        let pw = match (&cx.password, self.text.contains("{pw}")) {
            (Some(pw), _) => pw.clone(),
            (None, false) => String::new(),
            (None, true) => {
                actions.push(Action::Log(
                    "✘ 脚本用到 {pw}，但没有锁屏密码：请先在首页「解锁」里输入".into(),
                ));
                return;
            }
        };
        match rescue::parse_located(&self.text, &[("pw", &pw)]) {
            Ok(all) => cx.rescue.run_located(name, pick(&all, indices), fresh),
            Err(e) => actions.push(Action::Log(format!("✘ 脚本: {e}"))),
        }
    }

    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        height: f32,
        error_line: Option<usize>,
        next: Option<usize>,
        running: Option<usize>,
    ) {
        let font = theme::mono(12.5);
        let gutter_w = 46.0;
        // Exactly the workspace width: long lines scroll inside, they never
        // widen the page.
        let width = ui.available_width();
        egui::Frame::NONE
            .fill(theme::c().well)
            .stroke(egui::Stroke::new(1.0, theme::c().line))
            .corner_radius(theme::RADIUS)
            .show(ui, |ui| {
                ui.set_height(height);
                ui.set_width(width - 2.0);
                egui::ScrollArea::both()
                    .id_salt("wb-editor")
                    .auto_shrink(false)
                    .max_width(width - 2.0)
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.add_space(gutter_w);
                            let mut layouter = |ui: &egui::Ui,
                                                text: &dyn egui::TextBuffer,
                                                _wrap: f32|
                             -> Arc<egui::Galley> {
                                let job = highlight_job(text.as_str(), &font);
                                ui.ctx().fonts_mut(|f| f.layout_job(job))
                            };
                            let output = egui::TextEdit::multiline(&mut self.text)
                                .code_editor()
                                .frame(egui::Frame::NONE)
                                .margin(Margin::symmetric(8, 6))
                                .desired_width(width - gutter_w - 24.0)
                                .desired_rows(((height - 12.0) / 16.0).max(4.0) as usize)
                                .layouter(&mut layouter)
                                .show(ui);
                            if let Some(range) = output.cursor_range {
                                self.cursor_line = line_of(&self.text, range.primary.index.into());
                            }
                            paint_gutter(ui, &output, gutter_w, error_line, next, running);
                        });
                    });
            });
    }
}

/// The steps at `indices` (out-of-range ones skipped).
fn pick(all: &[Located], indices: &[usize]) -> Vec<Located> {
    indices
        .iter()
        .filter_map(|&i| all.get(i).cloned())
        .collect()
}

/// Line numbers, the error line (red), the next step (▶) and the running
/// line (●), aligned to the editor's laid-out rows.
fn paint_gutter(
    ui: &egui::Ui,
    output: &egui::text_edit::TextEditOutput,
    gutter_w: f32,
    error_line: Option<usize>,
    next: Option<usize>,
    running: Option<usize>,
) {
    let painter = ui.painter();
    let left = output.response.rect.left() - gutter_w;
    for (i, row) in output.galley.rows.iter().enumerate() {
        let line = i + 1;
        let y = output.galley_pos.y + row.pos.y + row.row.size.y / 2.0;
        let color = if Some(line) == error_line {
            theme::c().red
        } else if Some(line) == running || Some(line) == next {
            theme::c().amber
        } else {
            theme::c().text_faint
        };
        painter.text(
            egui::pos2(left + gutter_w - 8.0, y),
            egui::Align2::RIGHT_CENTER,
            line.to_string(),
            theme::mono(11.0),
            color,
        );
        let mark = if Some(line) == running {
            Some(("●", theme::c().green))
        } else if Some(line) == next {
            Some(("▶", theme::c().amber))
        } else {
            None
        };
        if let Some((glyph, color)) = mark {
            painter.text(
                egui::pos2(left + 6.0, y),
                egui::Align2::LEFT_CENTER,
                glyph,
                theme::mono(11.0),
                color,
            );
        }
    }
}

/// Syntax colours: comments faint, command names amber, arguments plain.
/// No wrapping, so editor rows are script lines (the gutter relies on it).
fn highlight_job(text: &str, font: &egui::FontId) -> LayoutJob {
    let c = theme::c();
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let fmt = |color: Color32| egui::TextFormat::simple(font.clone(), color);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            job.append("\n", 0.0, fmt(c.text));
        }
        if line.trim_start().starts_with('#') {
            job.append(line, 0.0, fmt(c.text_faint));
            continue;
        }
        for (j, part) in line.split(';').enumerate() {
            if j > 0 {
                job.append(";", 0.0, fmt(c.text_faint));
            }
            let split = part.find(':').map_or(part.len(), |k| k + 1);
            let (head, tail) = part.split_at(split);
            job.append(head, 0.0, fmt(c.amber));
            job.append(tail, 0.0, fmt(c.text));
        }
    }
    job
}

/// The new-profile form. `Some(Some(p))` = create, `Some(None)` = cancel.
fn new_profile_form(
    ui: &mut egui::Ui,
    form: &mut NewProfile,
    existing: &[Profile],
) -> Option<Option<Profile>> {
    let mut result = None;
    egui::Frame::NONE
        .fill(theme::c().surface)
        .stroke(egui::Stroke::new(1.0, theme::c().line))
        .corner_radius(theme::RADIUS)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.label(
                RichText::new("新建机型")
                    .font(theme::strong_font(15.0))
                    .color(theme::c().text),
            );
            ui.add_space(4.0);
            egui::Grid::new("new-profile")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("名称");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.name)
                            .hint_text("如：小米 14")
                            .desired_width(320.0),
                    );
                    ui.end_row();
                    ui.label("adb 型号");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.models)
                            .hint_text("ro.product.model，逗号分隔（可空）")
                            .desired_width(320.0),
                    );
                    ui.end_row();
                    ui.label("USB 接口");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.usb)
                            .hint_text("VID:PID:MI，逗号分隔（可空，稍后在配置里补）")
                            .font(theme::mono(11.5))
                            .desired_width(320.0),
                    );
                    ui.end_row();
                });
            if !form.candidates.is_empty() {
                ui.label(
                    RichText::new("当前插着的 USB 接口（点击加入；选 MTP / ADB 以外的空闲接口）：")
                        .color(theme::c().text_dim),
                );
                ui.horizontal_wrapped(|ui| {
                    for row in &form.candidates {
                        let id = row.target.to_string();
                        let label = format!("{id} {}", checkup::role_label(row.role));
                        if ui
                            .button(RichText::new(label).font(theme::mono(11.0)))
                            .on_hover_text(&row.description)
                            .clicked()
                            && !form.usb.contains(&id)
                        {
                            if !form.usb.trim().is_empty() {
                                form.usb.push_str(", ");
                            }
                            form.usb.push_str(&id);
                        }
                    }
                });
            }
            let check = check_new_profile(form, existing);
            if let Err(e) = &check {
                ui.label(RichText::new(e).color(theme::c().red));
            }
            ui.horizontal(|ui| {
                if widgets::primary(ui, "创建并开始编辑", check.is_ok()).clicked()
                    && let Ok(p) = check
                {
                    result = Some(Some(p));
                }
                if widgets::link(ui, "取消").clicked() {
                    result = Some(None);
                }
            });
        });
    result
}

/// Validate the form into a profile: unique name, valid USB ids; scripts
/// start from the generic unlock and the guide's template.
fn check_new_profile(form: &NewProfile, existing: &[Profile]) -> Result<Profile, String> {
    let name = form.name.trim();
    if name.is_empty() {
        return Err("请填写名称".into());
    }
    if name.contains(['[', ']']) {
        return Err("名称不能包含方括号".into());
    }
    if existing.iter().any(|p| p.name == name) {
        return Err(format!("已有名为「{name}」的机型"));
    }
    let usb: Vec<UsbTarget> = UsbTarget::parse_list(&form.usb)?;
    Ok(Profile {
        name: name.to_string(),
        models: form
            .models
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(String::from)
            .collect(),
        unlock: rescue::SCRIPT_UNLOCK.to_string(),
        enable: Some(guide::NEW_MODEL_TEMPLATE.trim_end().to_string()),
        usb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_show_next_step_when_idle_and_running_line_when_busy() {
        let steps = validate("wake\nwait:100\nclick:0.5,0.5").unwrap();
        assert_eq!(marker_lines(&steps, 1, None), (Some(2), None));
        assert_eq!(marker_lines(&steps, 1, Some(3)), (None, Some(3)));
        assert_eq!(marker_lines(&[], 0, None), (None, None));
    }

    #[test]
    fn cursor_offsets_map_to_lines() {
        let text = "wake\nenter\n\nback";
        assert_eq!(line_of(text, 0), 1);
        assert_eq!(line_of(text, 5), 2);
        assert_eq!(line_of(text, text.chars().count()), 4);
    }

    #[test]
    fn validated_and_real_parses_line_up_step_for_step() {
        // Indices chosen on the validated parse address the same steps in
        // the real one — including the 2nd command of a line.
        let text = "wake\ntext:{pw}; enter";
        let validated = validate(text).unwrap();
        let real = rescue::parse_located(text, &[("pw", "1234")]).unwrap();
        assert_eq!(validated.len(), real.len());
        let chosen = pick(&real, &[1, 2]);
        assert_eq!(chosen[0].step, rescue::Step::Text("1234".into()));
        assert_eq!(chosen[1].step, rescue::Step::Key(0, crate::aoa::KEY_ENTER));
        assert!(pick(&real, &[9]).is_empty());
    }

    #[test]
    fn highlighting_keeps_the_text_and_never_wraps() {
        let text = "# c\nclick:0.5,0.5; wait:100\n";
        let job = highlight_job(text, &theme::mono(12.0));
        assert_eq!(job.text, text);
        assert!(job.wrap.max_width.is_infinite());
    }

    fn form(name: &str, usb: &str) -> NewProfile {
        NewProfile {
            name: name.into(),
            models: "A1, B2".into(),
            usb: usb.into(),
            candidates: Vec::new(),
        }
    }

    #[test]
    fn new_profiles_are_checked_and_start_from_templates() {
        let existing = profiles::builtin();
        let p = check_new_profile(&form(" 小米 14 ", "2717:FF48"), &existing).unwrap();
        assert_eq!(p.name, "小米 14");
        assert_eq!(p.models, ["A1", "B2"]);
        assert_eq!(p.usb.len(), 1);
        rescue::parse(&p.unlock, &[("pw", "1")]).unwrap();
        rescue::parse(p.enable.as_deref().unwrap(), &[("pw", "1")]).unwrap();
        assert!(check_new_profile(&form("", ""), &existing).is_err());
        assert!(
            check_new_profile(&form("vivo Pad3", ""), &existing).is_err(),
            "duplicate"
        );
        assert!(check_new_profile(&form("X", "nope"), &existing).is_err());
        assert!(check_new_profile(&form("[X]", ""), &existing).is_err());
        let saved = profiles::parse(&profiles::render(&p)).unwrap();
        assert_eq!(saved, vec![p], "round-trips through the file");
    }
}
