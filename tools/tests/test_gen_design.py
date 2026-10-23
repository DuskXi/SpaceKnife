from __future__ import annotations

import json
from pathlib import Path

import pytest

import gen_design as g


def write(tmp: Path, name: str, data: dict[str, object]) -> None:
    (tmp / name).write_text(json.dumps(data), encoding="utf-8")


def test_repository_artefacts_are_up_to_date() -> None:
    assert g.stale(g.artefacts()) == [], "run: uv run python tools/gen_design.py"


def test_alias_resolution_and_type_inheritance(tmp_path: Path) -> None:
    write(
        tmp_path,
        "a.tokens.json",
        {
            "color": {
                "$type": "color",
                "base": {"$value": "#112233"},
                "alias": {"$value": "{color.base}"},
                "chain": {"$value": "{color.alias}"},
            },
            "size": {"$type": "dimension", "x": {"$value": {"value": 4, "unit": "px"}}},
        },
    )
    tokens = {t.dotted: t for t in g.load_tokens(tmp_path)}
    assert tokens["color.chain"].value == "#112233"
    assert tokens["color.chain"].type == "color"
    assert tokens["size.x"].value == 4


@pytest.mark.parametrize(
    ("data", "message"),
    [
        ({"c": {"$type": "color", "a": {"$value": "{c.b}"}, "b": {"$value": "{c.a}"}}}, "cycle"),
        ({"c": {"$type": "color", "a": {"$value": "{c.nope}"}}}, "unknown alias"),
        ({"c": {"$type": "color", "a": {"$value": "red"}}}, "#RRGGBB"),
        ({"c": {"a": {"$value": 1}}}, r"no \$type"),
        ({"c": {"$type": "gradient", "a": {"$value": 1}}}, "unsupported"),
        ({"c": {"$type": "dimension", "a": {"$value": {"value": 1, "unit": "rem"}}}}, "unit"),
    ],
)
def test_invalid_tokens_are_rejected(tmp_path: Path, data: dict[str, object], message: str) -> None:
    write(tmp_path, "x.tokens.json", data)
    with pytest.raises(g.TokenError, match=message):
        g.load_tokens(tmp_path)


def test_duplicate_top_level_group_is_rejected(tmp_path: Path) -> None:
    write(tmp_path, "a.tokens.json", {"c": {"$type": "number", "a": {"$value": 1}}})
    write(tmp_path, "b.tokens.json", {"c": {"$type": "number", "b": {"$value": 2}}})
    with pytest.raises(g.TokenError, match="defined twice"):
        g.load_tokens(tmp_path)


def test_naming() -> None:
    path = ("color", "text", "onAccent")
    assert g.kebab(path) == "color-text-on-accent"
    assert g.camel(path) == "colorTextOnAccent"
    assert g.pascal(("treemap", "level", "0", "headerBg")) == "TreemapLevel0HeaderBg"
    assert g.argb("#112233") == "#FF112233"
    assert g.argb("#11223344") == "#44112233"


def test_generated_python_module_matches_tokens() -> None:
    from spaceknife_client.tokens import THEMES, TOKENS, token

    assert TOKENS["color.bg.window"] == "#08090B"
    assert TOKENS["treemap.class.video"] == "#E07A1F"
    assert TOKENS["layout.toolbar"] == 48
    assert TOKENS["font.family.ui"][0] == "Geist"
    assert TOKENS["motion.duration.swap"] == 200
    assert TOKENS["visual.flamePolar.holeRatio"] == 0.24
    assert THEMES["dark"] is TOKENS
    assert THEMES["light"]["color.bg.window"] == token("color.bg.window", "light") == "#E9EBEF"
    assert THEMES["light"]["layout.toolbar"] == 48
    assert set(THEMES["light"]) == set(TOKENS)


# --------------------------------------------------------------------------- themes


BASE = {
    "c": {
        "$type": "color",
        "a": {"$value": "#112233"},
        "alias": {"$value": "{c.a}"},
        "g": {"b": {"$value": "#000000"}},
    },
    "n": {"$type": "number", "x": {"$value": 1}},
}


def write_theme(tmp: Path, override: dict[str, object]) -> None:
    write(tmp, "base.tokens.json", BASE)
    (tmp / "themes").mkdir(exist_ok=True)
    write(tmp / "themes", "light.tokens.json", override)


def test_theme_overrides_and_aliases_follow(tmp_path: Path) -> None:
    write_theme(tmp_path, {"$description": "notes", "c": {"a": {"$value": "#aabbcc", "$extensions": {}}}})
    dark = {t.dotted: t.value for t in g.load_tokens(tmp_path)}
    light = {t.dotted: t.value for t in g.load_tokens(tmp_path, "light")}
    assert (dark["c.a"], dark["c.alias"]) == ("#112233", "#112233")
    assert (light["c.a"], light["c.alias"], light["c.g.b"], light["n.x"]) == (
        "#AABBCC",
        "#AABBCC",
        "#000000",
        1,
    )
    themes = g.load_themes(tmp_path)
    assert g.themed(themes) == {"c.a", "c.alias"}
    css = g.emit_css(themes)
    # light on :root with everything; dark only for what differs, from the system or from data-theme
    root = css.split("\n}\n")[0]
    for line in ("color-scheme: light;", "--sk-n-x: 1;", "--sk-c-a: #AABBCC;"):
        assert line in root
    dark_body = "color-scheme: dark;\n{0}--sk-c-a: #112233;\n{0}--sk-c-alias: #112233;\n"
    assert (
        '@media (prefers-color-scheme: dark) {\n  :root:not([data-theme="light"]) {\n    '
        + dark_body.format("    ")
        + "  }\n}"
    ) in css
    assert '[data-theme="dark"] {\n  ' + dark_body.format("  ") + "}" in css
    assert (
        '[data-theme="light"] {\n  color-scheme: light;\n  --sk-c-a: #AABBCC;\n  --sk-c-alias: #AABBCC;\n}'
        in css
    )
    assert css.count("--sk-n-x") == 1


def test_native_artefacts_follow_the_system_theme(tmp_path: Path) -> None:
    # a Qt or WinUI frontend that sets nothing starts in the system's theme, light when it says
    # nothing (as tokens.css), and no themed key is missing in any theme
    write_theme(tmp_path, {"c": {"a": {"$value": "#aabbcc"}}})
    themes = g.load_themes(tmp_path)
    qml = g.emit_qml(themes)
    assert '    property string theme: Qt.styleHints.colorScheme === 2 ? "dark" : "light"' in qml
    assert 'readonly property color cA: theme === "light" ? "#FFAABBCC" : "#FF112233"' in qml
    assert 'readonly property color cGB: "#FF000000"' in qml
    xaml = g.emit_xaml(themes)
    # WinUI looks up "Light" / "Dark" and falls back to "Default" (high contrast too): dark is "Default"
    assert [m for m in ("Default", "Light", "Dark", "HighContrast") if f'x:Key="{m}"' in xaml] == [
        "Default",
        "Light",
    ]
    default, light = xaml.split('<ResourceDictionary x:Key="Light">')
    assert '<Color x:Key="SkCA">#FF112233</Color>' in default
    assert '<Color x:Key="SkCA">#FFAABBCC</Color>' in light
    assert xaml.count('x:Key="SkCGB"') == 1


@pytest.mark.parametrize(
    ("override", "message"),
    [
        ({"c": {"nope": {"$value": "#FFFFFF"}}}, "not in the base"),
        ({"x": {"$type": "color", "a": {"$value": "#FFFFFF"}}}, "not in the base"),
        ({"c": {"g": {"$value": "#FFFFFF"}}}, "token with a token"),
        ({"c": {"a": {"b": {"$value": "#FFFFFF"}}}}, "token with a token"),
        ({"c": {"a": {"$type": "dimension", "$value": {"value": 1, "unit": "px"}}}}, r"cannot change \$type"),
        ({"c": {"$type": "number"}}, r"cannot change \$type"),
        ({"n": {"x": {"$value": 2}}}, "only color and shadow"),
        ({"c": {"a": {"$value": "blue"}}}, "#RRGGBB"),
    ],
)
def test_invalid_theme_overrides_are_rejected(
    tmp_path: Path, override: dict[str, object], message: str
) -> None:
    write_theme(tmp_path, override)
    with pytest.raises(g.TokenError, match=message):
        g.load_tokens(tmp_path, "light")


def test_missing_or_unknown_theme(tmp_path: Path) -> None:
    write(tmp_path, "base.tokens.json", BASE)
    with pytest.raises(g.TokenError, match="not found"):
        g.load_tokens(tmp_path, "light")
    with pytest.raises(g.TokenError, match="unknown theme"):
        g.load_tokens(tmp_path, "sepia")


def _raw_base() -> dict[str, tuple[str, object, str | None]]:
    return g.collect(g.load_tree())


def test_light_theme_sets_every_colour_itself() -> None:
    # a colour added to the dark base without a light value would ship dark in the light theme
    override = json.loads((g.TOKENS_DIR / g.THEMES_SUBDIR / "light.tokens.json").read_text(encoding="utf-8"))
    written = set(g._token_paths(override))
    wanted = {
        name
        for name, (typ, value, _) in _raw_base().items()
        if typ in g.THEMED_TYPES and not (isinstance(value, str) and g.ALIAS.match(value))
    }
    assert wanted - written == set()


# the light values that are the dark ones on purpose (docs/UI-SPEC.md W7 待定 20): the six mid-tone
# class colours, the labels and dots on blocks, the selection rings, the reduced-motion badge, the
# branch palette's tiers 2 and 4, the tag markers' dark base, and the bright warning fill with its
# near-black text (status.warningFill is the dark warning colour; in light the warning text darkens)
SAME_AS_DARK = {
    *(f"treemap.class.{c}" for c in ("audio", "doc", "exec", "game", "image", "video")),
    "treemap.dot.onDark",
    "treemap.dot.onLight",
    *(f"treemap.label.{k}" for k in ("dark", "darkSub", "light", "lightSub")),
    "treemap.state.newBadgeBg",
    "treemap.state.newBadgeText",
    "treemap.state.selectOuter",
    "treemap.state.selectRing",
    *(f"visual.branch.{slot}.{tier}" for slot in range(1, 9) for tier in (2, 4)),
    "color.tag.base",
    "color.status.warningFill",
    "color.text.onWarning",
}


def test_light_theme_keeps_only_chosen_dark_values() -> None:
    themes = g.load_themes()
    coloured = {t.dotted for t in themes["dark"] if t.type in g.THEMED_TYPES}
    assert coloured - g.themed(themes) == SAME_AS_DARK


# --------------------------------------------------------------------------- contrast (both themes)


def _rgba(color: str) -> tuple[float, float, float, float]:
    r, g_, b = (int(color[i : i + 2], 16) / 255 for i in (1, 3, 5))
    return r, g_, b, (int(color[7:9], 16) / 255 if len(color) > 7 else 1.0)


def _luminance(color: str) -> float:
    def channel(c: float) -> float:
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4

    r, g_, b, _ = _rgba(color)
    return 0.2126 * channel(r) + 0.7152 * channel(g_) + 0.0722 * channel(b)


def _contrast(a: str, b: str) -> float:
    hi, lo = sorted((_luminance(a), _luminance(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def _over(top: str, under: str) -> str:
    """``top`` (with its alpha) composited over the opaque ``under`` in sRGB, as canvas and CSS do."""
    *t, a = _rgba(top)
    *u, _ = _rgba(under)
    return "#" + "".join(f"{round((x * a + y * (1 - a)) * 255):02X}" for x, y in zip(t, u, strict=True))


def _tint(color: str, pct: int, under: str) -> str:
    """CSS ``color-mix(in srgb, color pct%, transparent)`` over ``under``."""
    return _over(f"{color[:7]}{round(pct * 2.55):02X}", under)


def _mix(color: str, pct: int, base: str) -> str:
    """CSS ``color-mix(in srgb, color pct%, base)`` of an opaque ``color`` (premultiplied, as #RRGGBBAA)."""
    p = pct / 100
    *c, _ = _rgba(color)
    *b, ab = _rgba(base)
    alpha = p + (1 - p) * ab
    rgb = [(x * p + y * ab * (1 - p)) / alpha for x, y in zip(c, b, strict=True)]
    return "#" + "".join(f"{round(v * 255):02X}" for v in (*rgb, alpha))


THEME_NAMES = list(g.THEMES)


def _palette(theme: str) -> dict[str, str]:
    return {t.dotted: t.value for t in g.load_tokens(theme=theme) if t.type == "color"}


ACCENTS = ("default", "lime", "teal", "orange")
SURFACES = ("window", "chrome", "panel", "overlay", "raised", "hover", "active")
CLASSES = ("video", "image", "audio", "archive", "exec", "doc", "game", "code", "data", "other")

# (what, ratio, required ratio): text 4.5:1 (UI-MAIN-55; tertiary 5.6:1 on the surfaces,
# color.tokens.json), marks and outlines 3:1. A pair is measured as drawn: translucent colours over
# what they sit on, CSS color-mix() as in app.css.
Pair = tuple[str, float, float]


def _pair(what: str, fg: str, bg: str, need: float) -> Pair:
    return (f"{what}: {fg} on {bg}", _contrast(fg, bg), need)


def _chrome_pairs(T: dict[str, str]) -> list[Pair]:
    c = {k.removeprefix("color."): v for k, v in T.items() if k.startswith("color.")}
    rows: list[Pair] = []
    for text in ("primary", "secondary", "tertiary"):
        for s in (*SURFACES, "selected"):
            need = 5.6 if text == "tertiary" and s in SURFACES[:5] else 4.5
            rows.append(_pair(f"text.{text} on bg.{s}", c[f"text.{text}"], c[f"bg.{s}"], need))
    # accent tints under primary text: the current nav item / palette row / compare row, the
    # filter's current candidate, a search hit, the update card's option
    for pct, base in ((14, "panel"), (14, "overlay"), (30, "panel"), (14, "raised")):
        for a in ACCENTS:
            tint = _tint(c[f"accent.{a}"], pct, c[f"bg.{base}"])
            rows.append(_pair(f"text.primary on accent.{a} {pct}% / bg.{base}", c["text.primary"], tint, 4.5))
    for a in ACCENTS:
        accent = c[f"accent.{a}"]
        rows.append(_pair(f"text.onAccent on accent.{a}", c["text.onAccent"], accent, 4.5))  # UI-W7-31 ②
        # the ready update badge on hover: the accent 86 % toward white
        rows.append(
            _pair(f"text.onAccent on accent.{a} hovered", c["text.onAccent"], _over("#FFFFFF24", accent), 4.5)
        )
        for s in SURFACES[:6]:
            rows.append(_pair(f"accent.{a} as text on bg.{s}", accent, c[f"bg.{s}"], 4.5))
        for s in SURFACES:  # focus ring, a selected card's edge, a switch
            rows.append(_pair(f"accent.{a} as a mark on bg.{s}", accent, c[f"bg.{s}"], 3.0))
        # on its own tint: the title bar's update badge, 「当前」 on the about page, a remote chip
        for pct, s in ((16, "window"), (18, "panel"), (14, "window")):
            rows.append(
                _pair(f"accent.{a} on its {pct}% / bg.{s}", accent, _tint(accent, pct, c[f"bg.{s}"]), 4.5)
            )
        # the update badge hovered / open and downloading: text color-mix(accent 60 %, text.primary)
        mixed = _over(f"{c['text.primary']}66", accent)
        for pct in (26, 30):
            rows.append(
                _pair(
                    f"update badge text on accent.{a} {pct}%", mixed, _tint(accent, pct, c["bg.window"]), 4.5
                )
            )
    for st in ("success", "warning", "danger", "growth"):
        for s in SURFACES[:6]:
            rows.append(_pair(f"status.{st} on bg.{s}", c[f"status.{st}"], c[f"bg.{s}"], 4.5))
    # the warning's tints are of its fill (bright yellow in both themes) mixed into warningBase (none in
    # the dark theme, an opaque light yellow in the light one), its text is status.warning: the
    # plain-text chip (title bar), the compare warning, the update card's label, the log's warning level
    # (on a hovered row too), the help page's caution box; primary text on the settings note and the
    # remote-delete note
    fill, base = c["status.warningFill"], c["status.warningBase"]

    def warn_tint(pct: int, s: str) -> str:
        return _over(_mix(fill, pct, base), c[f"bg.{s}"])

    for pct, s in (
        (14, "window"),
        (14, "chrome"),
        (14, "panel"),
        (16, "overlay"),
        (22, "panel"),
        (22, "hover"),
        (8, "window"),
        (8, "panel"),
    ):
        rows.append(
            _pair(
                f"status.warning on warningFill {pct}% / bg.{s}", c["status.warning"], warn_tint(pct, s), 4.5
            )
        )
    # the plain-HTTP label on that chip: the fill's 18 % over it
    for s in ("window", "chrome"):
        label = _tint(fill, 18, warn_tint(14, s))
        rows.append(_pair(f"status.warning on the HTTP label / bg.{s}", c["status.warning"], label, 4.5))
    for pct, s in ((14, "window"), (12, "overlay")):
        rows.append(
            _pair(f"text.primary on warningFill {pct}% / bg.{s}", c["text.primary"], warn_tint(pct, s), 4.5)
        )
    rows.append(_pair("text.onWarning on status.warningFill", c["text.onWarning"], fill, 4.5))
    # the log's error level is mixed into the panel, also on a hovered row (app.css)
    for key, pct, s in (
        ("danger", 10, "overlay"),
        ("danger", 14, "panel"),
        ("danger", 22, "panel"),
        ("success", 18, "panel"),
        ("growth", 14, "panel"),
    ):
        color = c[f"status.{key}"]
        rows.append(
            _pair(f"status.{key} on its {pct}% / bg.{s}", color, _tint(color, pct, c[f"bg.{s}"]), 4.5)
        )
    rows.append(
        _pair("trash.outline on bg.raised (hover card)", T["treemap.trash.outline"], c["bg.raised"], 4.5)
    )
    # the status bar's capacity meter
    rows.append(_pair("meter.used on bg.chrome", c["meter.used"], c["bg.chrome"], 3.0))
    rows.append(_pair("status.success on bg.chrome", c["status.success"], c["bg.chrome"], 3.0))
    # tag shapes (UI-TM-49): on the shell's surfaces in the theme's colours; on the canvas, on their
    # dark base over the least fitting block, in the dark theme's colours (text.ts)
    blocks = [
        *(T[f"treemap.class.{k}"] for k in CLASSES),
        T["treemap.collapsed"],
        T["treemap.ground"],
        *(T[f"treemap.level.{k}.headerBg"] for k in range(4)),
        *(T[f"visual.branch.{slot}.{tier}"] for slot in range(1, 9) for tier in range(1, 5)),
    ]
    dark = _palette("dark")
    for tag in ("red", "yellow", "green", "blue"):
        for s in SURFACES:
            rows.append(_pair(f"tag.{tag} on bg.{s}", c[f"tag.{tag}"], c[f"bg.{s}"], 3.0))
        shape = dark[f"color.tag.{tag}"]
        worst = min((_over(c["tag.base"], b) for b in blocks), key=lambda bg: _contrast(shape, bg))
        rows.append(_pair(f"tag.{tag} (dark) on tag.base over a block", shape, worst, 3.0))
    return rows


def _treemap_pairs(T: dict[str, str]) -> list[Pair]:
    rows: list[Pair] = []
    for k in range(4):
        bg = T[f"treemap.level.{k}.headerBg"]
        rows.append(_pair(f"level.{k}.headerText", T[f"treemap.level.{k}.headerText"], bg, 4.5))
        rows.append(_pair(f"level.{k}.headerSub", T[f"treemap.level.{k}.headerSub"], bg, 4.5))
    rows.append(_pair("halo.text", T["treemap.halo.text"], T["treemap.halo.bg"], 4.5))
    rows.append(_pair("halo.sub", T["treemap.halo.sub"], T["treemap.halo.bg"], 4.5))
    lit = _over(T["visual.haloOverlay"], T["visual.center.bg"])
    for part in ("text", "sub"):
        rows.append(_pair(f"center.{part}", T[f"visual.center.{part}"], T["visual.center.bg"], 4.5))
        rows.append(_pair(f"center.{part} lit", T[f"visual.center.{part}"], lit, 4.5))
    for kind in ("free", "unknown", "pending"):
        for part in ("text", "sub"):
            for stripe in ("stripeA", "stripeB"):
                fg, bg = T[f"treemap.{kind}.{part}"], T[f"treemap.{kind}.{stripe}"]
                rows.append(_pair(f"{kind}.{part} on {stripe}", fg, bg, 4.5))
    rows.append(_pair("trash.badgeText", T["treemap.trash.badgeText"], T["treemap.trash.badgeBg"], 4.5))
    rows.append(
        _pair("state.newBadgeText", T["treemap.state.newBadgeText"], T["treemap.state.newBadgeBg"], 4.5)
    )
    rows.append(_pair("「新增」 badge", T["color.text.onAccent"], T["color.accent.default"], 4.5))
    for block in (
        "treemap.level.3.headerBg",
        "treemap.class.game",
        "treemap.class.data",
        "treemap.class.video",
        "treemap.ground",
    ):
        under = _over(T["treemap.state.deletedBadgeBg"], T[block])
        rows.append(_pair(f"「已删除」 badge over {block}", T["color.text.primary"], under, 4.5))
    # state layers drawn on the canvas (marks, 3:1): the change flash on the ground, the frames and
    # their headers; the breathing outline while scanning and the trash outline on its veil
    frames = ["treemap.ground", *(f"treemap.level.{k}.bg" for k in range(4))]
    for f in [*frames, *(f"treemap.level.{k}.headerBg" for k in range(4))]:
        rows.append(_pair(f"state.flash on {f}", T["treemap.state.flash"], T[f], 3.0))
    for f in frames:
        breathing = _over(f"{T['color.accent.default']}E6", T[f])
        rows.append(_pair(f"accent.default 90% on {f}", breathing, T[f], 3.0))
        veiled = _over(T["treemap.trash.veil"], T[f])
        rows.append(_pair(f"trash.outline on trash.veil over {f}", T["treemap.trash.outline"], veiled, 3.0))
    # the selection: the white ring or the dark ring outside it, whichever stands out more; the hover
    # inset against the block or against the seam outside it
    filled = [*(f"treemap.class.{k}" for k in CLASSES), "treemap.collapsed"]
    for f in [*frames, *filled]:
        bg = T[f]
        ring = max(
            _contrast(T["treemap.state.selectRing"], bg),
            _contrast(_over(T["treemap.state.selectOuter"], bg), bg),
        )
        rows.append((f"selection ring on {f}", ring, 3.0))
    for f in filled:
        inset = _over(T["treemap.state.hoverInset"], T[f])
        best = max(_contrast(inset, T[f]), _contrast(inset, T["treemap.level.0.bg"]))
        rows.append((f"hover inset on {f}", best, 3.0))
    return rows


@pytest.mark.parametrize("theme", THEME_NAMES)
def test_colour_pairs_are_readable(theme: str) -> None:
    T = _palette(theme)
    low = [
        f"{what} {ratio:.2f} < {need}"
        for what, ratio, need in _chrome_pairs(T) + _treemap_pairs(T)
        if ratio < need
    ]
    assert low == []


@pytest.mark.parametrize("theme", THEME_NAMES)
def test_level_contrast_keeps_the_levels_apart(theme: str) -> None:
    # 「层级对比」 (0–50) mixes each level's frame from level 0 by ×0–5 (treemap/paint.ts, clamped):
    # at the top of the slider no level may clip at white or black, and neighbours stay apart
    T = _palette(theme)
    base = _rgba(T["treemap.level.0.bg"])
    for times in (1, 2, 3, 5):
        frames = []
        for k in range(4):
            level = _rgba(T[f"treemap.level.{k}.bg"])
            mixed = [b + (v - b) * times for b, v in zip(base[:3], level[:3], strict=True)]
            assert all(0 <= v <= 1 for v in mixed), (theme, times, k)
            frames.append("#" + "".join(f"{round(v * 255):02X}" for v in mixed))
        assert all(_contrast(frames[k], frames[k - 1]) > 1.02 for k in range(1, 4)), (theme, times, frames)


LABELLED = [
    *(f"treemap.class.{c}" for c in CLASSES),
    "treemap.collapsed",
    *(f"treemap.diff.grow.{i}" for i in range(4)),
    *(f"treemap.diff.shrink.{i}" for i in range(3)),
    *(f"treemap.time.{i}" for i in range(5)),
    *(f"treemap.dupe.share.{i}" for i in range(1, 5)),
    "treemap.dupe.pending",
    "treemap.dupe.shared",
    *(f"visual.branch.{slot}.{tier}" for slot in range(1, 9) for tier in range(1, 5)),
    "visual.branch.neutral",
    *(f"visual.ring.{i}" for i in range(1, 5)),
]
# dark shortfalls from before the light theme (TODO.md): the size line on these, time.2's name, and
# the name on a lit third tier of the odd branches (4.31–4.44)
DARK_SHORT = {
    "treemap.time.2",
    "treemap.time.2 sub",
    *(f"visual.branch.{slot}.3 sub" for slot in (1, 3, 5, 7)),
    *(f"visual.branch.{slot}.3 lit" for slot in (1, 3, 5, 7)),
}


@pytest.mark.parametrize("theme", THEME_NAMES)
def test_block_labels_are_readable(theme: str) -> None:
    # labels take white or near-black, whichever contrasts more with the fill (UI-TM-19); the size
    # line is that colour at 86 % / 84 %; a flame graph's lit row keeps its labels' colours
    T = _palette(theme)

    def ink(fill: str) -> tuple[str, str]:
        light, dark = T["treemap.label.light"], T["treemap.label.dark"]
        if _contrast(fill, light) >= _contrast(fill, dark):
            return light, T["treemap.label.lightSub"]
        return dark, T["treemap.label.darkSub"]

    checks: list[tuple[str, str, str]] = []
    for key in LABELLED:
        fill = T[key]
        text, sub = ink(fill)
        checks += [(key, text, fill), (f"{key} sub", _over(sub, fill), fill)]
    # a lit row is a folder (flame graph, polar flame graph): on its ring tone, on any branch tier, or
    # on a compare tier (「其他」's grey for no change); it keeps its labels' colours, and its size line
    # takes the name's (flame/paint.ts, flamePolar/labels.ts)
    lit_keys = [
        *(f"visual.ring.{i}" for i in range(1, 5)),
        *(f"visual.branch.{slot}.{tier}" for slot in range(1, 9) for tier in range(1, 5)),
        "visual.branch.neutral",
        *(f"treemap.diff.grow.{i}" for i in range(4)),
        *(f"treemap.diff.shrink.{i}" for i in range(3)),
        *(f"treemap.dupe.share.{i}" for i in range(1, 5)),
        "treemap.collapsed",
    ]
    for key in lit_keys:
        text, _ = ink(T[key])
        checks.append((f"{key} lit", text, _over(T["visual.haloOverlay"], T[key])))
    low = {what for what, fg, bg in checks if _contrast(fg, bg) < 4.5}
    assert low == (DARK_SHORT if theme == "dark" else set())


@pytest.mark.parametrize("theme", THEME_NAMES)
def test_fills_are_distinct(theme: str) -> None:
    # a class without a token would be drawn in 「其他」's colour (lib/classes.ts)
    from spaceknife_protocol.classes import BUILTIN_CLASSES

    T = _palette(theme)
    fills = [T[f"treemap.class.{c.id}"] for c in BUILTIN_CLASSES]
    assert len(set(fills)) == len(fills)
    branch = {T[f"visual.branch.{slot}.{tier}"] for slot in range(1, 9) for tier in range(1, 5)}
    assert len(branch) == 32
    assert T["visual.center.bg"] == T["treemap.level.0.headerBg"]


def test_visualization_icons() -> None:
    _, icons = g.load_icons()
    assert {"vizTreemap", "vizFlame", "vizFlamePolar"} <= set(icons)
    assert (g.BUILD / "icons" / "viz-flame-polar.svg").exists()
