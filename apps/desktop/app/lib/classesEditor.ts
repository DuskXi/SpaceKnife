// The rows of 「设置 › 文件类别」 (pages/ClassesEditor.vue): the editable row of a class, its
// extensions, the extensions an earlier class already takes, and adding / moving a row (「其他」
// stays last).

import type { FilterClassInfo } from "@/protocol/generated";
import { t } from "@/lib/i18n";

export interface ClassRow {
  key: string;
  id: string | null;
  name: string;
  exts: string;
  /** the user's colours for the dark and the light theme, "" for the theme's */
  color: string;
  color_light: string;
}

// a new class's colour, the same in both themes (dark text on each, ≥ 5.9:1 in either; they stand
// apart from the built-in classes in both)
export const NEW_COLORS = ["#5EC8B8", "#D4FF3A", "#F2A65A", "#C77DFF", "#FF6B8A", "#4CC9F0"];
let nextKey = 1;

// what folders add to a built-in class (spaceknife.pathclass): shown under its extensions
export const folderNote = (id: string | null): string | null =>
  id === "game"
    ? t("另外，游戏安装目录里的文件都算这一类：Steam、Epic、GOG、Xbox、HoYoPlay 等启动器的游戏，Unity、Unreal、Godot 游戏，Minecraft，游戏存档")
    : id === "code"
      ? t("另外，项目文件夹里的文件都算这一类：含 .git、package.json、Cargo.toml、pyproject.toml、*.sln 等的文件夹")
      : id === "data"
        ? t("另外，模型缓存里的文件都算这一类：.ollama、.cache/huggingface、.lmstudio")
        : id === "exec"
          ? t("另外，系统卷信息和 NTFS 元数据算这一类；已安装的软件和应用数据（Program Files、AppData 等）里没有类别的文件也算")
          : null;

export const toRow = (c: FilterClassInfo): ClassRow => ({ key: c.id, id: c.id, name: c.name, exts: c.extensions.join("; "), color: c.color, color_light: c.color_light });

/** The extensions typed in a row: lower case, without dots, split at ; , or spaces. */
export const splitExts = (text: string): string[] =>
  text
    .split(/[;,\s]+/)
    .map((e) => e.trim().replace(/^\.+/, "").toLowerCase())
    .filter(Boolean);

/** Per row key, the extensions an earlier class already takes: that class colours them (UI-W7-33 重复提示). */
export function duplicates(rows: ClassRow[]): Map<string, string[]> {
  const seen = new Map<string, string>();
  const out = new Map<string, string[]>();
  for (const r of rows) {
    const mine: string[] = [];
    for (const e of splitExts(r.exts)) {
      const owner = seen.get(e);
      if (owner && owner !== r.name) mine.push(t("{ext}（{owner}）", { ext: e, owner }));
      else seen.set(e, r.name);
    }
    if (mine.length) out.set(r.key, mine);
  }
  return out;
}

/** Whether a row differs from the class the backend has (a new row always does). */
export function rowChanged(r: ClassRow, before: FilterClassInfo | undefined): boolean {
  return !(before && before.name === r.name.trim() && before.extensions.join("; ") === splitExts(r.exts).join("; ") && before.color === r.color && before.color_light === r.color_light);
}

/** A new class, before 「其他」. */
export function addRow(rows: ClassRow[]): ClassRow[] {
  const n = rows.length;
  const seed = NEW_COLORS[n % NEW_COLORS.length]!;
  const row: ClassRow = { key: `new-${nextKey++}`, id: null, name: t("新类别 {n}", { n }), exts: "", color: seed, color_light: seed };
  const other = rows.findIndex((r) => r.id === "other");
  const next = [...rows];
  next.splice(other < 0 ? next.length : other, 0, row);
  return next;
}

/** Row `from` put where row `to` is (before it); 「其他」 stays last. Null when nothing moves. */
export function moveRow(rows: ClassRow[], from: string, to: string): ClassRow[] | null {
  if (from === to) return null;
  const moved = rows.find((r) => r.key === from);
  if (!moved) return null;
  const next = rows.filter((r) => r.key !== from);
  const at = next.findIndex((r) => r.key === to);
  next.splice(at < 0 ? next.length : at, 0, moved);
  const other = next.findIndex((r) => r.id === "other");
  if (other >= 0 && other !== next.length - 1) next.push(next.splice(other, 1)[0]!);
  return next;
}

/** The row under the pointer while dragging: the one whose box holds `y`, else the nearest end. */
export function rowAt(boxes: { key: string; top: number; bottom: number }[], y: number): string | null {
  if (!boxes.length) return null;
  for (const b of boxes) if (y >= b.top && y < b.bottom) return b.key;
  return y < boxes[0]!.top ? boxes[0]!.key : boxes[boxes.length - 1]!.key;
}
