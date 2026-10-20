"""Update checks against the project's GitHub releases (``update.*``, capability ``updates.v1``).

The backend checks at most once a day (and on request), compares the newest release with the
running app's version and tells every session that announced ``updates.v1`` through
``update.changed``. How far behind the app is decides how loudly the front end says so:

- ``patch`` (0.3.0 → 0.3.1): a badge in the title bar only;
- ``minor`` (0.3.x → 0.4.0): also a card hanging at the top left with the summary;
- ``major`` (0.x → 1.0.0): also a dialog in the window.

Downloading is one click: the backend fetches the file for this kind of install into the user's
download folder and checks it against the release's ``SHA256SUMS*.txt``; the front end then opens
it (an installer, a disk image) or shows it in its folder (a portable archive).
"""

from __future__ import annotations

from datetime import datetime
from typing import Literal

from pydantic import Field

from .base import Params, Payload

UpdateLevel = Literal["none", "patch", "minor", "major"]
InstallKind = Literal["nsis", "portable", "appimage", "deb", "rpm", "dmg", "dev", "unknown"]
UpdateChannel = Literal["stable", "preview"]
DownloadStateName = Literal["idle", "downloading", "verifying", "done", "failed", "cancelled"]


class UpdateAsset(Payload):
    name: str = Field(description="File name, e.g. SpaceKnife-0.4.0-Setup-x64.exe")
    size: int = Field(ge=0, description="Bytes")
    url: str = Field(description="Its download URL on github.com")
    sha256: str | None = Field(
        default=None, description="From the release's SHA256SUMS*.txt; None when it publishes none"
    )


class UpdateInfo(Payload):
    version: str = Field(description="Without the leading v: 0.4.0, 0.4.0-beta.1")
    tag: str
    name: str = Field(description="The release title")
    published_at: datetime | None = None
    prerelease: bool = Field(description="Marked as a pre-release on GitHub")
    url: str = Field(description="The release page")
    notes: str = Field(default="", description="Release notes (Markdown), at most 16 KiB")
    asset: UpdateAsset | None = Field(
        default=None, description="The file for this kind of install; None: only the release page"
    )


class UpdateDownload(Payload):
    state: DownloadStateName = "idle"
    version: str | None = None
    received: int = Field(default=0, ge=0)
    total: int = Field(default=0, ge=0)
    path: str | None = Field(default=None, description="The downloaded file, once done")
    verified: bool | None = Field(
        default=None, description="SHA-256 matched (True), did not (never kept), no checksum published (None)"
    )
    error: str | None = Field(default=None, description="Why it failed, in the session's language")


class UpdateStatus(Payload):
    current: str = Field(description="Version of the running app")
    install_kind: InstallKind
    enabled: bool = Field(description="Automatic checks are on (setting general.update_check)")
    channel: UpdateChannel
    checking: bool = False
    checked_at: datetime | None = Field(default=None, description="Last successful check")
    error: str | None = Field(default=None, description="The last check failed: why (offline, rate limited)")
    level: UpdateLevel = Field(default="none", description="How far behind the newest release this app is")
    latest: UpdateInfo | None = Field(default=None, description="The newest release, when it is newer")
    dismissed: bool = Field(
        default=False, description="The card / dialog for this version was closed (the badge stays)"
    )
    download: UpdateDownload = Field(default_factory=UpdateDownload)


class UpdateVersionParams(Params):
    version: str = Field(min_length=1, max_length=64, description="The version shown (latest.version)")


class UpdateChangedEvent(Payload):
    status: UpdateStatus


__all__ = [
    "DownloadStateName",
    "InstallKind",
    "UpdateAsset",
    "UpdateChangedEvent",
    "UpdateChannel",
    "UpdateDownload",
    "UpdateInfo",
    "UpdateLevel",
    "UpdateStatus",
    "UpdateVersionParams",
]
