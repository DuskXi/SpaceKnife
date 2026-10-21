"""Entry point of the frozen backend (M8, PyInstaller onedir ``spaceknife-backend``).

    spaceknife-backend serve --token-stdin …     the backend CLI (``python -m spaceknife``)
    spaceknife-backend --helper --pipe … …       the scan helper (``python -m spaceknife.helper``)
    spaceknife-backend --apply-update <plan>      puts a portable copy's new version in place
                                                  (``python -m spaceknife.update_swap``)

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
    if sys.argv[1:2] == ["--apply-update"]:  # 「无缝更新」 of a portable copy: the standard library only
        from spaceknife.update_swap import main as apply_update

        sys.exit(apply_update(sys.argv[2:]))
    from spaceknife.cli import main as cli

    cli()


if __name__ == "__main__":
    main()
