"""Run the backend CLI with ``python -m spaceknife``."""

from .cli import main

if __name__ == "__main__":  # guard: spawn-context workers re-import __main__
    main()
