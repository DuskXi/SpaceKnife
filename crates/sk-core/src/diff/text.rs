//! User-facing texts of the compare (the Chinese source text is the message id, see `i18n`).

pub(crate) fn text(key: &str) -> &'static str {
    crate::i18n::t(match key {
        "cancelled" => "对比已取消",
        "direction" => "方向只能是 grow 或 shrink",
        "overlap" => "对比失败：树的子项范围相互重叠",
        "status.unchanged" => "不变",
        "status.added" => "新增",
        "status.removed" => "已删除",
        "status.changed" => "变化",
        "rest" => "{label} 中其余 {n} 项",
        _ => "",
    })
}
