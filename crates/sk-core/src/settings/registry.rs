//! The settings registry: names, descriptions, options, ranges, defaults, which page and group a
//! setting lives on, when it takes effect, and whether it can be changed on this platform / in
//! this build (UI-W7-27). Every text is the Chinese message id, translated when shown.

use sk_protocol::settings::{SettingApplies, SettingCategory, SettingControl, SettingValue};

const IS_WIN: bool = cfg!(windows);
const IS_MAC: bool = cfg!(target_os = "macos");

/// The type of a setting's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Str,
}

/// One setting.
#[derive(Clone, Debug)]
pub struct Spec {
    pub key: &'static str,
    pub category: SettingCategory,
    pub group: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub control: SettingControl,
    pub default: SettingValue,
    /// (value, label, why it cannot be chosen here — "" when it can)
    pub options: Vec<(&'static str, &'static str, &'static str)>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub unit: &'static str,
    pub applies: SettingApplies,
    pub synonyms: &'static [&'static str],
    /// Non-empty: shown disabled with this reason (and read-only)
    pub reason: &'static str,
    pub depends: Option<&'static str>,
    pub group_note: &'static str,
    pub kind: Kind,
}

impl Spec {
    fn new(
        key: &'static str,
        category: SettingCategory,
        group: &'static str,
        name: &'static str,
        description: &'static str,
        control: SettingControl,
        default: SettingValue,
    ) -> Spec {
        let kind = match default {
            SettingValue::Bool(_) => Kind::Bool,
            SettingValue::Int(_) => Kind::Int,
            _ => Kind::Str,
        };
        Spec {
            key,
            category,
            group,
            name,
            description,
            control,
            default,
            options: Vec::new(),
            min: None,
            max: None,
            step: None,
            unit: "",
            applies: SettingApplies::Now,
            synonyms: &[],
            reason: "",
            depends: None,
            group_note: "",
            kind,
        }
    }
    fn opts(mut self, o: &[(&'static str, &'static str)]) -> Spec {
        self.options = o.iter().map(|&(v, l)| (v, l, "")).collect();
        self
    }
    fn opts3(mut self, o: &[(&'static str, &'static str, &'static str)]) -> Spec {
        self.options = o.to_vec();
        self
    }
    fn range(mut self, min: f64, max: f64, step: f64) -> Spec {
        (self.min, self.max, self.step) = (Some(min), Some(max), Some(step));
        self
    }
    fn unit(mut self, u: &'static str) -> Spec {
        self.unit = u;
        self
    }
    fn applies(mut self, a: SettingApplies) -> Spec {
        self.applies = a;
        self
    }
    fn syn(mut self, s: &'static [&'static str]) -> Spec {
        self.synonyms = s;
        self
    }
    fn reason(mut self, r: &'static str) -> Spec {
        self.reason = r;
        self
    }
    fn depends(mut self, d: &'static str) -> Spec {
        self.depends = Some(d);
        self
    }
    fn note(mut self, n: &'static str) -> Spec {
        self.group_note = n;
        self
    }
}

/// The load levels (message ids: translated when shown).
pub const LOAD_LEVELS: &[(&str, &str)] = &[
    ("max", "极速"),
    ("balanced", "均衡 70%"),
    ("quiet", "安静 35%"),
    ("idle", "空闲"),
    ("custom", "自定义"),
];

/// The dark theme's presets, which also name the slot: the light theme has its own colour for each
/// (design/tokens color.accent.*), so the stored value stays the same when the theme changes.
pub const ACCENTS: [&str; 4] = ["#8B9CFF", "#D4FF3A", "#5EC8B8", "#F2A65A"];

const COLOR_MODES: &[(&str, &str)] =
    &[("class", "文件类别"), ("flat", "扁平"), ("time", "修改时间"), ("branch", "顶层分支")];
const SCAN_GROUP_NOTE: &str = "限制扫描造成的磁盘读取占用";

pub(crate) fn registry() -> Vec<Spec> {
    use SettingApplies::{NewViews, NextScan};
    use SettingCategory as C;
    use SettingControl::{Color, Number, Segmented, Slider, Switch, Text};
    let b = SettingValue::Bool;
    let i = SettingValue::Int;
    let s = |v: &str| SettingValue::Str(v.to_owned());
    vec![
        // ------------------------------------------------------------ 常规
        Spec::new("general.language", C::General, "语言", "界面语言", "切换后全部文字、日期与数字格式立即更换", Segmented, s("system"))
            .opts(&[("system", "跟随系统"), ("zh-CN", "简体中文"), ("en", "English")])
            .syn(&["language", "locale", "中文", "english"]),
        Spec::new("general.hover_keep_open", C::General, "悬停提示", "切换元素时保持打开", "鼠标从一个块移到另一个块时，提示卡直接换成新内容", Switch, b(true))
            .syn(&["tooltip", "hover"]),
        Spec::new("general.hover_dates", C::General, "悬停提示", "显示日期与时间", "提示卡显示创建、修改、访问时间", Switch, b(true))
            .syn(&["tooltip", "date", "time"]),
        Spec::new("general.hover_age", C::General, "悬停提示", "显示文件年龄", "用「2 个月前」这样的相对时间显示", Switch, b(true))
            .syn(&["tooltip", "age", "relative"]),
        Spec::new("general.flash_on_done", C::General, "窗口", "扫描结束时若窗口在后台则闪烁提醒", "Windows 任务栏闪烁、macOS 程序坞图标弹跳、Linux 窗口紧急提示", Switch, b(true))
            .syn(&["notify", "flash", "taskbar"]),
        Spec::new("general.log_auto_open", C::General, "日志", "有新事件时自动打开日志面板", "收到警告或错误时自动展开日志面板；与日志面板里的开关是同一个设置", Switch, b(true))
            .syn(&["log", "console", "popup"]),
        Spec::new("general.log_scan_warnings", C::General, "日志", "记录扫描警告", "例如「无法访问 …：拒绝访问」", Switch, b(true))
            .syn(&["log", "denied", "warning"]),
        // ------------------------------------------------------------ 关于与更新 (the keys stay general.*)
        Spec::new("general.update_check", C::About, "更新", "自动检查更新", "每天最多一次，只读取 GitHub 上本项目的发布列表，不发送任何使用数据；小版本只在右上角提示", Switch, b(true))
            .syn(&["update", "upgrade", "release", "version", "升级", "新版本"]),
        Spec::new("general.update_download", C::About, "更新", "自动下载更新", "发现新版本后在后台下载并校验；只有你在更新卡片里点了「无缝更新」或安装方式才会安装，从不自己安装", Switch, b(true))
            .depends("general.update_check")
            .syn(&["update", "download", "auto", "下载"]),
        Spec::new("general.update_channel", C::About, "更新", "提示哪些版本", "预览版是 GitHub 上标为预发布（Pre-release）的版本", Segmented, s("preview"))
            .opts(&[("preview", "包括预览版"), ("stable", "只要正式版")])
            .syn(&["update", "channel", "beta", "prerelease", "预发布"]),
        // ------------------------------------------------------------ 外观
        Spec::new("appearance.theme", C::Appearance, "主题", "主题", "跟随系统时按系统的浅色 / 深色设置切换", Segmented, s("system"))
            .opts(&[("system", "跟随系统"), ("light", "浅色"), ("dark", "深色")])
            .syn(&["theme", "dark", "light"]),
        Spec::new("appearance.accent", C::Appearance, "主题", "强调色", "主要按钮、当前导航项、开关、滑块和焦点环", Color, s(ACCENTS[0]))
            .opts(&[(ACCENTS[0], ACCENTS[0]), (ACCENTS[1], ACCENTS[1]), (ACCENTS[2], ACCENTS[2]), (ACCENTS[3], ACCENTS[3])])
            .syn(&["accent", "color", "colour"]),
        Spec::new("appearance.reduce_motion", C::Appearance, "动画", "减少动画", "开启后缩放改为瞬时切换，扫描中的条纹不滚动", Segmented, s("system"))
            .opts(&[("system", "跟随系统"), ("on", "开"), ("off", "关")])
            .syn(&["motion", "animation", "accessibility"]),
        Spec::new("appearance.level_contrast", C::Appearance, "树图视觉", "层级对比", "相邻层级底色的差别（平 ↔ 深）", Slider, i(10))
            .range(0.0, 50.0, 1.0)
            .syn(&["contrast", "depth"]),
        Spec::new("appearance.border_contrast", C::Appearance, "树图视觉", "边框对比", "块边框的强弱（柔和 ↔ 锐利）", Slider, i(10))
            .range(0.0, 50.0, 1.0)
            .syn(&["border"]),
        Spec::new("appearance.halo_levels", C::Appearance, "树图视觉", "高亮光晕层数", "悬停或选中时点亮几层祖先标题栏（克制 ↔ 绚烂）", Slider, i(3))
            .range(0.0, 10.0, 1.0)
            .syn(&["halo", "glow", "highlight"]),
        Spec::new("appearance.hover_highlight", C::Appearance, "树图视觉", "悬停高亮", "鼠标下的元素加一圈内描边（深色主题白色，浅色主题近黑色）", Switch, b(true))
            .syn(&["hover", "highlight"]),
        Spec::new("appearance.mouse_trail", C::Appearance, "树图视觉", "鼠标拖尾", "鼠标划过的元素渐隐", Switch, b(false))
            .syn(&["trail"]),
        Spec::new("appearance.trash_effect", C::Appearance, "树图视觉", "回收站特效", "回收站 / 废纸篓里的东西蒙上一层暗纹、围上虚线，回收站本身标出清空后能释放多少；「动态」时虚线流动、偶尔扫过一道光（「减少动画」时不动）", Segmented, s("animated"))
            .opts(&[("animated", "动态"), ("static", "静态"), ("off", "关")])
            .syn(&["trash", "recycle bin", "回收站", "废纸篓"]),
        Spec::new("appearance.selection_shadow", C::Appearance, "树图视觉", "选中投影", "选中元素带投影，更容易追踪", Switch, b(true))
            .syn(&["shadow", "selection"]),
        Spec::new("appearance.default_color_mode", C::Appearance, "配色样式", "树图默认配色", "与工具栏的「类别｜扁平｜时间｜分支」对应", Segmented, s("class"))
            .opts(COLOR_MODES)
            .applies(NewViews)
            .syn(&["color", "flat", "classes", "branch"]),
        Spec::new("appearance.default_visual", C::Appearance, "可视化", "新视图默认可视化", "与工具栏的「树图｜火焰图｜火焰-极坐标图」对应", Segmented, s("treemap"))
            .opts(&[("treemap", "树图"), ("flame", "火焰图"), ("flame_polar", "火焰-极坐标图")])
            .applies(NewViews)
            .syn(&["visualization", "treemap", "flame", "flame_polar", "polar"]),
        Spec::new("appearance.partition_color_mode", C::Appearance, "可视化", "火焰图和火焰-极坐标图默认配色", "新视图切到火焰图或火焰-极坐标图时使用", Segmented, s("branch"))
            .opts(COLOR_MODES)
            .applies(NewViews)
            .syn(&["color", "flame", "polar", "branch"]),
        // ------------------------------------------------------------ 树图
        Spec::new("treemap.font_scale", C::Treemap, "几何", "元素字号", "标题栏、条和环的高度以及标签显示阈值随字号等比缩放", Slider, i(100))
            .range(80.0, 140.0, 5.0)
            .unit("%")
            .syn(&["font", "size"]),
        Spec::new("treemap.min_element", C::Treemap, "几何", "最小元素尺寸", "小于这个尺寸的元素并入「N 个小项」聚合块", Slider, i(10))
            .range(3.0, 20.0, 1.0)
            .unit("px")
            .syn(&["minimum", "small items", "geometry"]),
        Spec::new("treemap.aspect", C::Treemap, "几何", "元素比例", "只影响树图：偏高 ↔ 偏宽；0 表示尽量接近正方形", Slider, i(0))
            .range(-60.0, 60.0, 5.0)
            .syn(&["aspect", "ratio"]),
        Spec::new("treemap.initial_detail", C::Treemap, "几何", "初始细节层级", "新视图的细节层级，与工具栏「层级」一致", Slider, i(5))
            .range(1.0, 15.0, 1.0)
            .applies(NewViews)
            .syn(&["detail", "depth", "level"]),
        Spec::new("treemap.show_free", C::Treemap, "几何", "新视图显示可用空间", "整卷视图里显示可用空间块", Switch, b(true))
            .applies(NewViews)
            .syn(&["free space"]),
        Spec::new("treemap.show_unknown", C::Treemap, "几何", "新视图显示未知空间", "整卷视图里显示未知空间块", Switch, b(true))
            .applies(NewViews)
            .syn(&["unknown space"]),
        Spec::new("treemap.show_trash", C::Treemap, "几何", "新视图显示回收站", "扫描里有回收站 / 废纸篓时显示它；关掉后回收站不占画面，其余的块放大填满", Switch, b(true))
            .applies(NewViews)
            .syn(&["trash", "recycle bin", "回收站", "废纸篓"]),
        Spec::new("treemap.sort_by_size", C::Treemap, "几何", "按大小排序", "同一文件夹内大块排在左上", Switch, b(true))
            .syn(&["sort", "order"]),
        Spec::new("treemap.flame_origin", C::Treemap, "火焰图", "火焰图方向", "根所在的一侧", Segmented, s("bottom"))
            .opts(&[("top", "根在上"), ("bottom", "根在下")])
            .syn(&["flame", "direction", "orientation", "root", "icicle"]),
        Spec::new("treemap.zoom_ms", C::Treemap, "动画", "缩放动画时长", "双击放大或返回时的过渡时长；0 表示无动画", Slider, i(250))
            .range(0.0, 600.0, 25.0)
            .unit("ms")
            .syn(&["animation", "zoom", "duration"]),
        Spec::new("treemap.fps", C::Treemap, "动画", "动画帧率", "动画重绘的频率", Segmented, s("display"))
            .opts(&[("display", "跟随显示器"), ("60", "60 fps"), ("30", "30 fps")])
            .syn(&["fps", "frame rate"]),
        Spec::new("treemap.dynazoom", C::Treemap, "动画", "缩放时实时重排（DynaZoom）", "缩放过程中元素实时重新排列", Switch, b(false))
            .reason("DynaZoom 还没有实现（见 TODO）")
            .syn(&["dynazoom"]),
        Spec::new("treemap.dynasmooth", C::Treemap, "动画", "按交互调节帧率（DynaSmooth）", "只在操作时使用高帧率，更省电", Switch, b(true))
            .syn(&["dynasmooth"]),
        Spec::new("treemap.flash_changes", C::Treemap, "变化提示", "元素变化闪烁", "扫描中或文件变化时元素闪烁提示", Switch, b(true))
            .syn(&["flash", "blink", "changes"]),
        // ------------------------------------------------------------ 扫描与性能
        Spec::new("scan.method", C::Scan, "扫描方式", "默认扫描方式", "自动与快速 MFT：整个 NTFS 卷用 MFT 快速扫描（需要时请求一次管理员授权，拒绝则改用硬扫描），其他位置硬扫描", Segmented, s("auto"))
            .opts3(&[
                ("auto", "自动", ""),
                ("mft", "快速 MFT", if IS_WIN { "" } else { "只在 Windows 的 NTFS 卷上可用" }),
                ("hard", "硬扫描", ""),
            ])
            .applies(NextScan)
            .syn(&["mft", "fast", "hard", "method"]),
        Spec::new("scan.elevate", C::Scan, "扫描方式", "需要时请求管理员权限", "仅启动只读扫描助手，主程序不提权；关闭后没有管理员权限时直接改用硬扫描", Switch, b(true))
            .reason(if IS_WIN { "" } else { "只在 Windows 的 NTFS 卷上可用" })
            .syn(&["admin", "uac", "elevate", "helper"]),
        Spec::new("scan.load_manual", C::Scan, "磁盘负载上限与并发", "手动扫描默认", "从启动页开始的扫描与重新扫描", Segmented, s("balanced"))
            .opts(LOAD_LEVELS)
            .applies(NextScan)
            .note(SCAN_GROUP_NOTE)
            .syn(&["load", "limit", "throttle", "io"]),
        Spec::new("scan.load_incremental", C::Scan, "磁盘负载上限与并发", "增量重扫默认", "实时监控发现变化后的定点重扫", Segmented, s("quiet"))
            .opts(LOAD_LEVELS)
            .note(SCAN_GROUP_NOTE)
            // nothing reads this yet: the monitor's rescans wait for the running scan's governor only
            .reason("还没有接入：实时监控的定点重扫不按这一档限速")
            .syn(&["load", "incremental"]),
        Spec::new("scan.custom_limit", C::Scan, "磁盘负载上限与并发", "自定义上限", "选择「自定义」档位时使用", Slider, i(70))
            .range(5.0, 100.0, 5.0)
            .unit("%")
            .applies(NextScan)
            .note(SCAN_GROUP_NOTE)
            .syn(&["custom", "limit"]),
        Spec::new("scan.idle_low_io", C::Scan, "磁盘负载上限与并发", "空闲时使用系统低 I/O 优先级", "「空闲」档位使用系统低 I/O 优先级，不与前台程序抢磁盘", Switch, b(true))
            .applies(NextScan)
            .note(SCAN_GROUP_NOTE)
            .syn(&["io priority", "idle", "background"]),
        Spec::new("scan.yield_when_busy", C::Scan, "磁盘负载上限与并发", "整盘繁忙时让路", "其他程序让整块磁盘超过上限时，扫描降到最低进度", Switch, b(true))
            .applies(NextScan)
            .note(SCAN_GROUP_NOTE)
            .syn(&["yield", "busy"]),
        Spec::new("scan.concurrency", C::Scan, "磁盘负载上限与并发", "最大并发", "自动：按处理器核数选择；手动：按下面的数值", Segmented, s("auto"))
            .opts(&[("auto", "自动"), ("manual", "手动")])
            .applies(NextScan)
            .note(SCAN_GROUP_NOTE)
            .syn(&["concurrency", "threads", "workers", "parallel"]),
        Spec::new("scan.workers", C::Scan, "磁盘负载上限与并发", "手动并发数", "手动时最多同时列出几个文件夹", Number, i(4))
            .range(1.0, 64.0, 1.0)
            .applies(NextScan)
            .depends("scan.concurrency=manual")
            .note(SCAN_GROUP_NOTE)
            .syn(&["concurrency", "workers"]),
        Spec::new("scan.ads", C::Scan, "扫描范围", "扫描 NTFS 备用数据流", "MFT 快速扫描时把附加在文件上的隐藏数据流计入大小（硬扫描不读取数据流）", Switch, b(false))
            .applies(NextScan)
            .reason(if IS_WIN { "" } else { "只在 Windows 上可用" })
            .syn(&["ads", "alternate data stream", "stream"]),
        Spec::new("scan.cross_mounts", C::Scan, "扫描范围", "跨越挂载点", "进入挂载在文件夹下的其他卷；关闭时只统计当前卷", Switch, b(false))
            .applies(NextScan)
            .syn(&["mount", "mount point", "volume"]),
        Spec::new("scan.protected_places", C::Scan, "扫描范围", "受保护的位置", "没有「完全磁盘访问权限」时，macOS 会为桌面、文稿、下载、云盘、照片图库和其他应用的数据逐个弹窗询问", Segmented, s("ask"))
            .opts(&[("ask", "扫描前询问"), ("skip", "跳过，不弹窗"), ("allow", "照常访问")])
            .applies(NextScan)
            .reason(if IS_MAC { "" } else { "只在 macOS 上需要" })
            .syn(&["full disk access", "privacy", "tcc", "permission", "prompt", "完全磁盘访问"]),
        Spec::new("scan.hardlinks", C::Scan, "统计口径", "硬链接计算", "只计一次时，合计与磁盘实际占用一致", Segmented, s("once"))
            .opts(&[("once", "只计一次"), ("each", "每个链接都计")])
            .applies(NextScan)
            .syn(&["hardlink", "hard link", "inode"]),
        Spec::new("scan.unit_base", C::Scan, "统计口径", "大小单位", "按 1000 计时，778.6 GB 显示为 836.0 GB", Segmented, s("system"))
            .opts(&[("system", "跟随系统"), ("1024", "1024"), ("1000", "1000")])
            .syn(&["unit", "1024", "1000", "kib", "gib", "base"]),
        Spec::new(
            "scan.monitor",
            C::Scan,
            "实时监控",
            "实时监控",
            if IS_WIN {
                "使用 USN 日志 + ReadDirectoryChangesEx，变化的文件会闪烁提示"
            } else if IS_MAC {
                "使用 FSEvents，变化的文件会闪烁提示"
            } else {
                "使用 inotify，变化的文件会闪烁提示"
            },
            Switch,
            b(true),
        )
        .syn(&["monitor", "watch", "usn", "inotify", "fsevents", "hooks"]),
        Spec::new("scan.coalesce_ms", C::Scan, "实时监控", "事件合并间隔", "此时间内的连续变化合并为一次刷新", Number, i(150))
            .range(50.0, 5000.0, 10.0)
            .unit("ms")
            .depends("scan.monitor")
            .syn(&["debounce", "coalesce", "interval"]),
        // ------------------------------------------------------------ 标签
        Spec::new("tags.name_red", C::Tags, "显示名", "红色标签 ▲", "显示名出现在详情面板和悬停卡里；过滤语法仍写 :red", Text, s(""))
            .syn(&["tag", "red", "label"]),
        Spec::new("tags.name_yellow", C::Tags, "显示名", "黄色标签 ■", "过滤语法仍写 :yellow", Text, s(""))
            .syn(&["tag", "yellow"]),
        Spec::new("tags.name_green", C::Tags, "显示名", "绿色标签 ●", "过滤语法仍写 :green", Text, s(""))
            .syn(&["tag", "green"]),
        Spec::new("tags.name_blue", C::Tags, "显示名", "蓝色标签 ◆", "过滤语法仍写 :blue", Text, s(""))
            .syn(&["tag", "blue"]),
        Spec::new("tags.omit_ctrl", C::Tags, "快捷键", "树图有焦点时可省略 Ctrl", "直接按 1–4 打标签、按 0 清除", Switch, b(true))
            .syn(&["shortcut", "ctrl", "tag key"]),
        Spec::new("tags.remember", C::Tags, "保存", "记住标签", "按扫描根路径保存标签，重新扫描或打开快照后恢复", Switch, b(true))
            .syn(&["persist", "save tags"]),
        // ------------------------------------------------------------ 快捷键
        Spec::new("keys.preset", C::Keys, "键位预设", "键位预设", "改了任意一个键后显示为「自定义」", Segmented, s("modern"))
            .opts(&[("modern", "现代"), ("classic", "SpaceSniffer 经典")])
            .reason("改键还没有实现（见 TODO）；下表列出当前键位")
            .syn(&["keymap", "shortcuts", "hotkeys", "classic"]),
        // ------------------------------------------------------------ 高级
        Spec::new("advanced.log_level", C::Advanced, "日志", "日志文件级别", "写入日志文件的最低级别", Segmented, s("INFO"))
            .opts(&[("INFO", "信息"), ("DEBUG", "调试")])
            .syn(&["log", "debug", "verbose"]),
        Spec::new("advanced.auto_snapshot", C::Advanced, "缓存与快照", "扫描后自动保存快照", "供启动页的最近记录和快照对比使用", Switch, b(true))
            .applies(NextScan)
            .syn(&["snapshot", "cache", "autosave"]),
        Spec::new("advanced.snapshot_keep", C::Advanced, "缓存与快照", "保留最近快照数", "更早的快照每周只保留一份", Number, i(10))
            .range(1.0, 100.0, 1.0)
            .syn(&["snapshot", "retention", "keep"]),
        Spec::new("advanced.map_large_scans", C::Advanced, "内存", "大扫描存进映射文件", "开启后，节点表超过 256 MB（约 200 万项）的扫描存进磁盘映射文件，不占私有内存；代价是扫完后每项约写盘 200 字节", Switch, b(false))
            .applies(NextScan)
            .syn(&["memory", "ram", "mmap", "large", "内存映射"]),
        Spec::new("advanced.memory_budget", C::Advanced, "内存", "扫描数据内存上限", "扫描数据超过物理内存的这个比例后，新的节点表存进磁盘映射文件：不占私有提交，常用部分仍由系统留在内存里", Number, i(25))
            .range(5.0, 90.0, 5.0)
            .unit("%")
            .applies(NextScan)
            .syn(&["memory", "ram", "mmap", "budget", "swap", "内存映射"]),
    ]
}
