//! 说明 tab: the complete guide to writing and debugging rescue scripts,
//! inside the app. The command reference is rendered from
//! `rescue::COMMANDS` (the same table the tests check against the parser),
//! so the guide cannot drift from what the scripts accept.

use crate::rescue::COMMANDS;
use crate::theme;
use eframe::egui::{self, Margin, RichText};

/// The built-in vivo Pad3 workflow, one line per action, with what each
/// line does. Joined, it is exactly `rescue::SCRIPT_ENABLE_DEBUGGING`
/// (tested), so the example is always the real thing.
pub const PAD3_ANNOTATED: &[(&str, &str)] = &[
    ("wake; wait:500", "唤醒屏幕，等动画结束"),
    ("swipe:0,-700; wait:700", "上滑，进入锁屏密码界面"),
    (
        "text:{pw}; enter; wait:2500",
        "输入锁屏密码并确认；等解锁和「USB 已连接」对话框出现",
    ),
    ("rot:1", "之后的点击按横屏计算（Pad3 横屏使用）"),
    (
        "click:0.41,0.61; wait:500",
        "点 USB 用途「连接办公套件」：平板自己打开 USB 调试并切换 USB 模式",
    ),
    (
        "reconnect:20000; wait:1500",
        "平板断开后以新 USB 模式重连（最多等 20 秒）",
    ),
    (
        "click:0.5,0.582; wait:800",
        "关掉「需打开电脑的 vivo 办公套件」提示（知道了）",
    ),
    (
        "click:0.3625,0.525; wait:400",
        "「允许 USB 调试吗？」勾选「一律允许使用这台计算机进行调试」",
    ),
    (
        "click:0.5,0.601; wait:600",
        "点「允许」：电脑上出现已授权的 adb 设备，可以投屏了",
    ),
];

/// A starting point for a new model (phone, portrait).
pub const NEW_MODEL_TEMPLATE: &str = "\
# 1. 解锁（与「解锁」按钮相同）
wake; wait:500
swipe:0,-700; wait:700
text:{pw}; enter; wait:2500
# 2. 竖屏手机：点击按竖屏计算
rot:0
# 3. 先用 move 对准，确认后改成 click
move:0.5,0.88
";

/// What the guide asks the caller to do.
pub enum Action {
    /// Put this text into the script editor (replacing it).
    Insert(String),
}

pub fn ui(ui: &mut egui::Ui) -> Option<Action> {
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("guide")
        .auto_shrink(false)
        .show(ui, |ui| {
            // Readable line length, never wider than the workspace.
            let width = ui.available_width().min(760.0);
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.y = 8.0;

            heading(ui, "救援脚本是什么");
            para(
                ui,
                "设备屏幕坏了、USB 调试也没开时，电脑把自己模拟成 USB 键盘和触控笔，按脚本一步步盲操作：解锁、选 USB 模式、允许调试。\
                 脚本按机型保存在「机型配置」里：unlock 是「解锁」按钮执行的脚本，enable 是「一键救援」执行的脚本。",
            );
            para(
                ui,
                "机型自动选择的顺序：插着的设备以前用 adb 记下的型号 → 当前 adb 设备的型号 → 配置里 usb 接口能对上的机型 → 通用 Android。也可以在「机型」下拉框里手动选。",
            );

            heading(ui, "命令参考");
            para(ui, "命令用分号或换行分隔；坐标都是屏幕比例（0~1），左上角是 0,0，正中是 0.5,0.5。");
            command_table(ui);

            heading(ui, "开发一个新机型");
            steps(
                ui,
                &[
                    "准备：找一台同型号、屏幕完好的设备（或坏屏机开着调试时先投屏看画面），打开 开发者选项 →「指针位置」，屏幕顶部会显示触点坐标。",
                    "在「脚本」页点「新建机型」：填名称；adb 型号和 USB 接口会从当前连接的设备和「设备体检」自动带入。",
                    "从模板开始写解锁部分，用「单步」一行一行执行，确认每一步设备的反应。",
                    "量坐标：用 move:x,y 让笔悬停，看「指针位置」显示的位置是否在按钮上；不对就改数字再运行这一行。对准后把 move 改成 click。也可以截图，按钮位置 ÷ 截图宽高。",
                    "设备切换 USB 模式（例如选「文件传输」或厂商的「办公套件」）时会断开重连：在那次点击后面加 reconnect:20000，脚本会等设备回来再继续。",
                    "整段写好后点「运行全部」完整跑一遍，再「保存到机型配置」。回到「救援」页，用「一键救援」验证。",
                ],
            );
            if ui
                .button(RichText::new("把新机型模板放进编辑器").color(theme::c().amber))
                .clicked()
            {
                action = Some(Action::Insert(NEW_MODEL_TEMPLATE.into()));
            }

            heading(ui, "调试");
            bullets(
                ui,
                &[
                    "运行全部（F5）：从头执行整个脚本，屏幕方向从横屏（rot:1）开始。",
                    "单步（F10）：只执行下一步；行号旁的 ▶ 是下一步，● 是正在执行的行。单步会保留之前 rot: 设定的方向。",
                    "运行当前行（Ctrl+Enter）：只执行光标所在的那一行——改了坐标后马上试。",
                    "停止（Esc）：在下一步或等待中停下；设备保持接管，可以接着单步。",
                    "编辑时实时检查语法：有错的行号标红，下方显示原因，修好前不能运行。",
                    "每次运行的结果在底部「日志」里（点开可看全部）；{pw} 用首页「解锁」里输入或已保存的锁屏密码，日志里不会出现密码。",
                ],
            );

            heading(ui, "常见问题");
            faq(ui, "点击位置偏了 / 点到了别处", "检查 rot：竖屏手机用 rot:0，横屏用 rot:1。坐标是相对当前显示方向的比例。");
            faq(ui, "密码里的中文或特殊字符没输入", "USB 键盘只能输入英文、数字和常见符号；日志会提示有几个字符无法输入。");
            faq(ui, "屏幕在等待时熄灭了", "等待期间会每 2 秒自动按一下 Shift 保持亮屏；如果设备锁屏超时很短，在关键步骤前加 wake。");
            faq(ui, "「没有找到可用的设备」", "打开「救援 → 设备体检」看电脑识别到的 USB 接口；机型配置里的 usb = VID:PID:MI 要和它一致（每种 USB 模式一个）。Windows 第一次会请求管理员权限安装驱动，点「是」。");
            faq(ui, "切换 USB 模式后脚本中断", "在触发切换的点击后加 reconnect:毫秒；新模式的接口也要写进 usb。");
            faq(ui, "检测到多台设备", "为避免误操作，救援只在连接一台可救援设备时工作。");

            heading(ui, "示例：vivo Pad3 一键救援（内置）");
            for (code, meaning) in PAD3_ANNOTATED {
                code_row(ui, code, meaning, None);
            }
            if ui
                .button(RichText::new("把这个示例放进编辑器").color(theme::c().amber))
                .clicked()
            {
                action = Some(Action::Insert(pad3_script()));
            }
            ui.add_space(12.0);
        });
    action
}

/// The annotated Pad3 lines as one script, one action per line.
pub fn pad3_script() -> String {
    PAD3_ANNOTATED
        .iter()
        .map(|(code, _)| *code)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Width of the syntax column; summaries wrap in the rest.
const SYNTAX_W: f32 = 230.0;

fn command_table(ui: &mut egui::Ui) {
    let mut group = "";
    for doc in COMMANDS {
        if doc.group != group {
            group = doc.group;
            ui.add_space(4.0);
            ui.label(
                RichText::new(group)
                    .font(theme::ui(11.5))
                    .color(theme::c().text_faint),
            );
        }
        code_row(ui, doc.syntax, doc.summary, Some(doc.example));
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        RichText::new(text)
            .font(theme::strong_font(15.0))
            .color(theme::c().text),
    );
}

fn para(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::c().text_dim));
}

/// A marker (number / bullet) and text that wraps at the page width.
fn marked(ui: &mut egui::Ui, marker: &str, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label(
            RichText::new(marker)
                .font(theme::mono(12.0))
                .color(theme::c().amber),
        );
        ui.add(egui::Label::new(RichText::new(text).color(theme::c().text)).wrap());
    });
}

fn steps(ui: &mut egui::Ui, items: &[&str]) {
    for (i, item) in items.iter().enumerate() {
        marked(ui, &format!("{}.", i + 1), item);
    }
}

fn bullets(ui: &mut egui::Ui, items: &[&str]) {
    for item in items {
        marked(ui, "·", item);
    }
}

/// A code / explanation row: fixed-width code column, wrapping text.
fn code_row(ui: &mut egui::Ui, code: &str, text: &str, example: Option<&str>) {
    egui::Frame::NONE
        .fill(theme::c().surface)
        .corner_radius(theme::RADIUS)
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                // Top-down inside, so the code wraps at the column width.
                ui.allocate_ui_with_layout(
                    egui::vec2(SYNTAX_W, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(SYNTAX_W);
                        ui.add(
                            egui::Label::new(
                                RichText::new(code)
                                    .font(theme::mono(11.0))
                                    .color(theme::c().amber),
                            )
                            .wrap(),
                        );
                    },
                );
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(text).color(theme::c().text)).wrap());
                    if let Some(example) = example {
                        ui.label(
                            RichText::new(format!("例：{}", example.replace('\n', "  |  ")))
                                .font(theme::mono(10.5))
                                .color(theme::c().text_faint),
                        );
                    }
                });
            });
        });
}

fn faq(ui: &mut egui::Ui, question: &str, answer: &str) {
    egui::Frame::NONE
        .fill(theme::c().surface)
        .stroke(egui::Stroke::new(1.0, theme::c().line))
        .corner_radius(theme::RADIUS)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(question).color(theme::c().text).strong());
            ui.label(RichText::new(answer).color(theme::c().text_dim));
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rescue;

    #[test]
    fn pad3_annotation_is_exactly_the_builtin_script() {
        let vars = [("pw", "1234")];
        assert_eq!(
            rescue::parse(&pad3_script(), &vars).unwrap(),
            rescue::parse(rescue::SCRIPT_ENABLE_DEBUGGING, &vars).unwrap()
        );
    }

    #[test]
    fn the_new_model_template_parses() {
        rescue::parse(NEW_MODEL_TEMPLATE, &[("pw", "1")]).unwrap();
    }
}
