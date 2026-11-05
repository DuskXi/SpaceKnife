//! Text helpers of the export module over [`crate::i18n`] and [`crate::fmt`].

/// The message id translated into the interface language.
pub(crate) fn tr(id: &str) -> String {
    crate::i18n::t(id).to_owned()
}

/// The interface language's tag (`zh-CN`, `en`).
pub(crate) fn language() -> &'static str {
    crate::i18n::language().as_str()
}

/// `str.format` with named string fields over a translated message.
pub(crate) fn fill(template: &str, fields: &[(&str, &str)]) -> String {
    let args: Vec<(&str, crate::i18n::Arg)> = fields.iter().map(|(k, v)| (*k, (*v).into())).collect();
    crate::i18n::format(template, &args)
}

pub(crate) use crate::fmt::UNIT_BASE;

/// `fmt.size(n)`.
pub(crate) fn size(n: i64) -> String {
    crate::fmt::size(n as f64)
}

/// `f"{n:,}"`.
pub(crate) fn thousands(n: i64) -> String {
    crate::fmt::count(n)
}
