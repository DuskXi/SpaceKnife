"""Entry point of the frozen backend (M8, PyInstaller onedir ``spaceknife-backend``).

    spaceknife-backend serve --token-stdin …     the backend CLI (``python -m spaceknife``)
    spaceknife-backend --helper --pipe … …       the scan helper (``python -m spaceknife.helper``)

A frozen app has no ``-m``: the helper re-runs this executable with ``--helper`` first
(``spaceknife.helper.launch.helper_command``), and the spawn-context scan workers re-run it
through ``multiprocessing.freeze_support()``.
"""

import multiprocessing
import sys


def main() -> None:
    multiprocessing.freeze_support()  # a scan worker: runs its task and exits here
    if sys.argv[1:2] == ["--helper"]:
        from spaceknife.helper.__main__ import main as helper

        sys.exit(helper(sys.argv[2:]))
    from spaceknife.cli import main as cli

    cli()


if __name__ == "__main__":
    main()
