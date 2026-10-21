// The newest release (pre-releases included: GitHub's /releases/latest skips them), read from the
// GitHub API once per build. The workflow passes GITHUB_TOKEN; without network the fallback below
// (the current release) keeps the build working.

export const REPO = "DuskXi/SpaceKnife";
export const REPO_URL = `https://github.com/${REPO}`;

export type AssetKind = "windows" | "portableWin" | "macArm64" | "macX64" | "deb" | "rpm" | "appimage" | "portableLinux" | "sums";

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
  tag: "v0.3.0",
  version: "0.3.0",
  url: `${REPO_URL}/releases/tag/v0.3.0`,
  date: "2026-10-19T00:00:00Z",
  prerelease: true,
  assets: {
    windows: { kind: "windows", name: "SpaceKnife-0.3.0-Setup-x64.exe", size: 142080672, url: download("v0.3.0", "SpaceKnife-0.3.0-Setup-x64.exe"), sha256: "a155931fc38d92d483957c010a4f0eb83fd5be89a29e184ed6a357a2efc3a1c9" },
    deb: { kind: "deb", name: "spaceknife_0.3.0_amd64.deb", size: 137770172, url: download("v0.3.0", "spaceknife_0.3.0_amd64.deb"), sha256: "e6ed0520bb1534e805d925d5376f10d3cfd18709bab97e0174ffa83f7976c6d7" },
    rpm: { kind: "rpm", name: "spaceknife-0.3.0.x86_64.rpm", size: 124011489, url: download("v0.3.0", "spaceknife-0.3.0.x86_64.rpm"), sha256: "ddda0eb6126d747f41e4fab220d6ba390cbbe80b1b5724e9f4eb9f2d198bb379" },
    appimage: { kind: "appimage", name: "SpaceKnife-0.3.0-x86_64.AppImage", size: 171635170, url: download("v0.3.0", "SpaceKnife-0.3.0-x86_64.AppImage"), sha256: "78c10339906969056579330672e9d4a72b5af4af6ee167e1093b832145c9f78e" },
    sums: { kind: "sums", name: "SHA256SUMS.txt", size: 383, url: download("v0.3.0", "SHA256SUMS.txt") },
  },
};

function kindOf(name: string): AssetKind | null {
  if (/-Setup-x64\.exe$/i.test(name)) return "windows";
  if (/-win-x64-portable\.zip$/i.test(name)) return "portableWin";
  if (/-linux-x64-portable\.tar\.gz$/i.test(name)) return "portableLinux";
  if (/_amd64\.deb$/i.test(name)) return "deb";
  if (/\.x86_64\.rpm$/i.test(name)) return "rpm";
  if (/\.AppImage$/i.test(name)) return "appimage";
  if (/-mac-arm64\.dmg$/i.test(name)) return "macArm64";
  if (/-mac-x64\.dmg$/i.test(name)) return "macX64";
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
