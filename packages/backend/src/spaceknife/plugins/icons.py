"""Cleaning a plugin's SVG icon: 24 × 24, shapes only, ``currentColor``, no scripts or references."""

from __future__ import annotations

import re
import xml.etree.ElementTree as ET

MAX_BYTES = 8 << 10
# The app's own icons (design/icons/icons.json; a test keeps the two equal): what ``icon = "…"`` names
APP_ICONS = frozenset(
    {
        "back",
        "basket",
        "bolt",
        "check",
        "chevronDown",
        "chevronRight",
        "clock",
        "close",
        "command",
        "copy",
        "diff",
        "diskGauge",
        "drive",
        "eye",
        "file",
        "filter",
        "folder",
        "forward",
        "gauge",
        "globe",
        "home",
        "info",
        "layers",
        "log",
        "logo",
        "minus",
        "more",
        "palette",
        "pause",
        "play",
        "plus",
        "pulse",
        "refresh",
        "reveal",
        "search",
        "settings",
        "shield",
        "sidebar",
        "snapshot",
        "split",
        "stop",
        "tag",
        "terminal",
        "thermometer",
        "up",
        "upload",
        "vizFlame",
        "vizFlamePolar",
        "vizTreemap",
        "warning",
        "windowClose",
        "windowMaximize",
        "windowMinimize",
    }
)
SVG_NS = "http://www.w3.org/2000/svg"
ELEMENTS = frozenset({"svg", "path", "line", "circle", "rect", "polyline", "polygon", "ellipse", "g"})
GEOMETRY = frozenset(
    {
        "d",
        "x",
        "y",
        "x1",
        "y1",
        "x2",
        "y2",
        "cx",
        "cy",
        "r",
        "rx",
        "ry",
        "width",
        "height",
        "points",
        "transform",
    }
)
STYLE = frozenset({"stroke-width", "stroke-linecap", "stroke-linejoin", "fill-rule", "clip-rule", "opacity"})
PAINT = frozenset({"fill", "stroke"})
_NUMBERS = re.compile(r"^[0-9eE.,\s+\-]*$")
_PATH = re.compile(r"^[MmLlHhVvCcSsQqTtAaZz0-9eE.,\s+\-]*$")
_TRANSFORM = re.compile(r"^(?:\s*(?:translate|scale|rotate|matrix|skewX|skewY)\([0-9eE.,\s+\-]*\)\s*)*$")


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def sanitize_svg(text: str) -> str:
    """The icon rebuilt from the allowed elements and attributes; ValueError when it is not an icon."""
    if len(text.encode("utf-8")) > MAX_BYTES:
        raise ValueError(f"an icon is at most {MAX_BYTES >> 10} KiB")
    if "<!ENTITY" in text or "<!DOCTYPE" in text:
        raise ValueError("no DOCTYPE or entities in an icon")
    try:
        root = ET.fromstring(text)
    except ET.ParseError as exc:
        raise ValueError(f"not valid SVG: {exc}") from exc
    if _local(root.tag) != "svg":
        raise ValueError("the root element must be <svg>")
    if root.get("viewBox", "").split() != ["0", "0", "24", "24"]:
        raise ValueError('viewBox must be "0 0 24 24"')

    def clean(el: ET.Element, depth: int) -> str:
        tag = _local(el.tag)
        if tag not in ELEMENTS:
            raise ValueError(f"<{tag}> is not allowed in an icon")
        if depth > 8:
            raise ValueError("an icon nests too deeply")
        attrs: list[str] = []
        for key, value in el.attrib.items():
            name = _local(key)
            if name in ("viewBox", "xmlns") and tag == "svg":
                continue
            if name in GEOMETRY:
                pattern = _PATH if name == "d" else _TRANSFORM if name == "transform" else _NUMBERS
                if not pattern.match(value):
                    raise ValueError(f"{name}={value!r} is not allowed")
            elif name in STYLE:
                if not re.fullmatch(r"[a-z0-9.\-]+", value):
                    raise ValueError(f"{name}={value!r} is not allowed")
            elif name in PAINT:
                if value not in ("currentColor", "none"):
                    raise ValueError(f"{name} must be currentColor or none")
            else:
                raise ValueError(f"the attribute {name!r} is not allowed")
            attrs.append(f'{name}="{value}"')
        inner = "".join(clean(child, depth + 1) for child in el)
        if (el.text or "").strip():
            raise ValueError("no text in an icon")
        head = " ".join([tag, *attrs])
        if tag == "svg":
            head = f'svg xmlns="{SVG_NS}" viewBox="0 0 24 24"' + (" " + " ".join(attrs) if attrs else "")
        return f"<{head}>{inner}</{tag}>" if inner else f"<{head}/>"

    return clean(root, 0)


__all__ = ["sanitize_svg"]
