"""File operations (docs/manage.md §2): everything that changes the user's files lives here.

* :mod:`.paths` — the four copy formats of a path; the built-in protected paths per platform.
* :mod:`.basket` — the delete basket (held by the backend only): containment, small-item groups,
  freeable space with cloud placeholders and hard links, refresh after tree changes.
* :mod:`.locks` — which processes hold files open (Restart Manager, ``/proc``, ``lsof``).
* :mod:`.trash` — move to the trash (never a permanent deletion in disguise), restore, delete
  permanently; one result per item with an error code and a Chinese reason.
* :mod:`._win` — the Win32 / COM plumbing behind them (imported on Windows only).

The modules are imported where they are used (nothing is re-exported here), so loading the
package does not load ctypes plumbing nobody needs.
"""
