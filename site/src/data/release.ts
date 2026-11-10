// The newest release (pre-releases included: GitHub's /releases/latest skips them), read from the
// GitHub API once per build. The workflow passes GITHUB_TOKEN; without network the fallback below
// (a published release: 0.3.4 until 0.3.5 is out, then 0.3.5's real files) keeps the build working.

export const REPO = "DuskXi/SpaceKnife";
export const REPO_URL = `https://github.com/${REPO}`;

export type AssetKind = "windows" | "portableWin" | "macArm64" | "macX64" | "deb" | "rpm" | "appimage" | "portableLinux" | "portableLinuxTauri" | "sums";

export interface Asset {
  kind: AssetKind;
  name: string;
  size: number;
  url: string;
  sha256?: string;
}

export interface Release {
  tag: string;
  version: string;
  url: string;
  date: string; // ISO
  prerelease: boolean;
  assets: Partial<Record<AssetKind, Asset>>;
}

const download = (tag: string, name: string) => `${REPO_URL}/releases/download/${tag}/${name}`;

const FALLBACK: Release = {
  tag: "v0.3.4",
  version: "0.3.4",
  url: `${REPO_URL}/releases/tag/v0.3.4`,
  date: "2026-10-26T01:58:38Z",
  prerelease: true,
  assets: {
    windows: { kind: "windows", name: "SpaceKnife-0.3.4-Setup-x64.exe", size: 143153798, url: download("v0.3.4", "SpaceKnife-0.3.4-Setup-x64.exe"), sha256: "383f640a26e81948496594abf3ddbc7a0242bd5e2e7a2b155832b6c33d8acb40" },
    portableWin: { kind: "portableWin", name: "SpaceKnife-0.3.4-win-x64-portable.zip", size: 183082031, url: download("v0.3.4", "SpaceKnife-0.3.4-win-x64-portable.zip"), sha256: "913688e791550f26083e7de927e88c4bb84003b64a17e163610a96e664a1fccc" },
    macArm64: { kind: "macArm64", name: "SpaceKnife-0.3.4-mac-arm64.dmg", size: 148145991, url: download("v0.3.4", "SpaceKnife-0.3.4-mac-arm64.dmg"), sha256: "4871bf56b3a8d2b9eaa9db80e59a187244adeb5774d55e8140c7b14c5231abe1" },
    macX64: { kind: "macX64", name: "SpaceKnife-0.3.4-mac-x64.dmg", size: 155463753, url: download("v0.3.4", "SpaceKnife-0.3.4-mac-x64.dmg"), sha256: "ae9e8c8cbad76a3d0106af2e2aa4ee6c58de16350ca30291b53ac939e09b8972" },
    deb: { kind: "deb", name: "spaceknife_0.3.4_amd64.deb", size: 138477424, url: download("v0.3.4", "spaceknife_0.3.4_amd64.deb"), sha256: "78724d934dc4b59bb933cc7179be1c4c13411c5f97b4000785dd169a9d82e0bf" },
    rpm: { kind: "rpm", name: "spaceknife-0.3.4.x86_64.rpm", size: 124584069, url: download("v0.3.4", "spaceknife-0.3.4.x86_64.rpm"), sha256: "5d684e067b9b78ca1fb5d1c68422e093bfc57b7a78c40dd3bff5525962701525" },
    appimage: { kind: "appimage", name: "SpaceKnife-0.3.4-x86_64.AppImage", size: 172401471, url: download("v0.3.4", "SpaceKnife-0.3.4-x86_64.AppImage"), sha256: "7308df3cccca3851258347ed2d786940f8b4d76cddff059d518f427aa4d0f75f" },
    portableLinux: { kind: "portableLinux", name: "SpaceKnife-0.3.4-linux-x64-portable.tar.gz", size: 169897705, url: download("v0.3.4", "SpaceKnife-0.3.4-linux-x64-portable.tar.gz"), sha256: "c0fa2e138d978bed80bd57682cc3dd85941ceb722614aa33daa9ca33ffa3d64f" },
    sums: { kind: "sums", name: "SHA256SUMS.txt", size: 383, url: download("v0.3.4", "SHA256SUMS.txt"), sha256: "719e75850e57952b96680b6119d75fae286e508af912028d7e4b9db7c5be0adf" },
  },
};

// Every release keeps 0.3.4's file names (SpaceKnife-<v>-Setup-x64.exe, SpaceKnife-<v>-mac-arm64.dmg, …;
// docs/packaging.md §9); Tauri's own bundler names (SpaceKnife_<v>_x64-setup.exe, SpaceKnife_<v>_aarch64.dmg)
// are accepted too, for a build attached unrenamed.
function kindOf(name: string): AssetKind | null {
  if (/(?:-Setup-x64|_x64-setup)\.exe$/i.test(name)) return "windows";
  if (/-win-x64-portable\.zip$/i.test(name)) return "portableWin";
  // Linux portable: under 0.3.4's name the Electron shell (any glibc 2.34+ system, RHEL 9 too),
  // from 0.3.5 also the far smaller Tauri app, which needs the system's WebKitGTK 4.1
  if (/-linux-x64-portable\.tar\.gz$/i.test(name)) return "portableLinux";
  if (/-linux-x64-tauri\.tar\.gz$/i.test(name)) return "portableLinuxTauri";
  if (/_amd64\.deb$/i.test(name)) return "deb";
  if (/\.x86_64\.rpm$/i.test(name)) return "rpm";
  if (/\.AppImage$/i.test(name)) return "appimage";
  if (/(?:-mac-arm64|_aarch64)\.dmg$/i.test(name)) return "macArm64";
  if (/(?:-mac-x64|_x64)\.dmg$/i.test(name)) return "macX64";
  if (/^SHA256SUMS\.txt$/i.test(name)) return "sums"; // the installers' (portable and macOS files: their own digests)
  return null;
}

interface ApiAsset {
  name: string;
  size: number;
  browser_download_url: string;
  digest?: string | null;
}

interface ApiRelease {
  tag_name: string;
  html_url: string;
  published_at: string;
  prerelease: boolean;
  draft: boolean;
  assets: ApiAsset[];
}

async function fetchLatest(): Promise<Release> {
  const headers: Record<string, string> = { Accept: "application/vnd.github+json", "User-Agent": "spaceknife-site" };
  const token = process.env.GITHUB_TOKEN;
  if (token) headers.Authorization = `Bearer ${token}`;
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=10`, { headers, signal: AbortSignal.timeout(15000) });
    if (!res.ok) throw new Error(`GitHub API ${res.status}`);
    const list = (await res.json()) as ApiRelease[];
    const rel = list.find((r) => !r.draft && r.assets.some((a) => kindOf(a.name) === "windows"));
    if (!rel) throw new Error("no release with installers");
    const assets: Release["assets"] = {};
    for (const a of rel.assets) {
      const kind = kindOf(a.name);
      if (!kind) continue;
      const sha = a.digest?.startsWith("sha256:") ? a.digest.slice(7) : undefined;
      assets[kind] = { kind, name: a.name, size: a.size, url: a.browser_download_url, sha256: sha };
    }
    return { tag: rel.tag_name, version: rel.tag_name.replace(/^v/, ""), url: rel.html_url, date: rel.published_at, prerelease: rel.prerelease, assets };
  } catch (err) {
    console.warn(`[release] using the built-in release info (${String(err)})`);
    return FALLBACK;
  }
}

let cached: Promise<Release> | undefined;

/** The release the download buttons point at (fetched once per build). */
export function latestRelease(): Promise<Release> {
  cached ??= fetchLatest();
  return cached;
}

export function fmtSize(bytes: number): string {
  return `${Math.round(bytes / 2 ** 20)} MB`;
}
