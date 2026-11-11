// A `folders` setting's value (treemap.hidden_folders, components/pages/FolderList.vue): the folders
// one per line, as the backend keeps them (sk-core settings folder_list: trimmed, no empty lines,
// each once).

/** The folders of the value, in their order. */
export function folderLines(value: string | null | undefined): string[] {
  const out: string[] = [];
  for (const line of String(value ?? "").split(/\r?\n/)) {
    const p = line.trim();
    if (p && !out.includes(p)) out.push(p);
  }
  return out;
}

/** The value with `paths` added after the folders it has (each once); `added`: any was new. */
export function addFolders(value: string | null | undefined, paths: readonly string[]): { value: string; added: boolean } {
  const list = folderLines(value);
  let added = false;
  for (const raw of paths) {
    const p = raw.trim();
    if (p && !list.includes(p)) {
      list.push(p);
      added = true;
    }
  }
  return { value: list.join("\n"), added };
}

/** A folder's name (its last part) and the path of the folder it is in ("" for a drive or /). */
export function splitPath(p: string): { name: string; dir: string } {
  const trimmed = p.replace(/[\\/]+$/, "");
  const at = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
  if (at < 0 || !trimmed) return { name: p, dir: "" };
  return { name: trimmed.slice(at + 1) || p, dir: trimmed.slice(0, at + 1) };
}

/** A full path on the computer the files are on: a drive or a share on Windows (C:\…, \\server\share),
 * from / elsewhere. A relative path or ~ names no folder of a scan. */
export function isFullPath(p: string, windows: boolean): boolean {
  return windows ? /^[A-Za-z]:[\\/]/.test(p) || /^\\\\[^\\]+\\[^\\]+/.test(p) : p.startsWith("/");
}
