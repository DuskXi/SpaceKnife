"""Update checks against the project's GitHub releases (``update.*``, capability ``updates.v1``).

The backend checks at most once a day (and on request), compares the newest release with the
running app's version and tells every session that announced ``updates.v1`` through
``update.changed``. How far behind the app is decides how loudly the front end says so:

- ``patch`` (0.3.0 → 0.3.1): a badge in the title bar only;
- ``minor`` (0.3.x → 0.4.0): also a card hanging at the top left with the summary;
- ``major`` (0.x → 1.0.0): also a dialog in the window.

Closing the card or the dialog (``update.dismiss``) keeps them away for that version.
``update.mute`` turns off every prompt of a series: a minor one (``"0.4"``: every 0.4.x) or a major
one (``"1"``: every 1.x). For a release of a muted series the front end shows no card, dialog or
badge, and nothing is downloaded by itself; the 「关于与更新」 page still shows it and turns the
prompts back on.

Downloading is one click, or happens by itself (setting ``general.update_download``): the backend
fetches the file for this kind of install into the ``updates`` folder of its data directory and
checks it against the release's ``SHA256SUMS*.txt``. Installing it is always the user's click, one
of two ways: 「无缝更新」 (the Windows installer run silently; a portable copy's files swapped by the
new version's backend, ``spaceknife.update_swap``; an AppImage replaced in place, then the new
version starts) or the classic way (the installer with its pages, the disk image or package opened,
or the file shown in its folder). Nothing is ever installed by itself. The backend removes what the
folder no longer needs when it starts.

``update.releases`` lists the recent releases with their notes for the timeline of the
「关于与更新」 settings page.
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
    series: str | None = Field(
        default=None,
        description="The series of the newest release that update.mute takes: its major.minor for a"
        ' minor update ("0.4"), its major for a major one ("1"); None for a patch release or no update',
    )
    muted: bool = Field(
        default=False,
        description="The user turned off the prompts of that series: no card, dialog or badge, no automatic"
        " download (the 「关于与更新」 page still shows it)",
    )
    download: UpdateDownload = Field(default_factory=UpdateDownload)
    next_check_at: datetime | None = Field(
        default=None,
        description="When the next automatic check is due; None while automatic checks are off, and where"
        " the backend makes none (tests, demos, tools)",
    )


class UpdateReleases(Payload):
    releases: list[UpdateInfo] = Field(
        description="The recent releases, newest first (at most 30, notes at most 16 KiB each, no asset)"
    )


class UpdateVersionParams(Params):
    version: str = Field(min_length=1, max_length=64, description="The version shown (latest.version)")


class UpdateMuteParams(Params):
    series: str = Field(
        pattern=r"^\d{1,6}(\.\d{1,6})?$",
        description='A series as UpdateStatus.series names it: "0.4" (every 0.4.x) or "1" (every 1.x)',
    )
    muted: bool = Field(default=True, description="False turns its prompts back on")


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
    "UpdateMuteParams",
    "UpdateReleases",
    "UpdateStatus",
    "UpdateVersionParams",
]
