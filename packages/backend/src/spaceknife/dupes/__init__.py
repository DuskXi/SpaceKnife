"""Duplicate files (docs/dupes.md): files whose whole contents were read and found identical.

- :mod:`.select` S0 (candidates from the store, no IO) and the plan;
- :mod:`.reads` the IO stages, native (``spaceknife_native``) or in Python;
- :mod:`.engine` the job: S1 identity, S1b block maps, S2 head and tail, S3 checkpointed BLAKE3;
- :mod:`.cache` the per-volume hash cache; :mod:`.identity` volume keys and trust;
- :mod:`.results` groups, roles, ledger, rollup, frame columns; :mod:`.keep` the keep rules.
"""

from .engine import Job, JobOptions
from .keep import KeepOptions
from .results import Results
from .select import ScanInput, Scope

__all__ = ["Job", "JobOptions", "KeepOptions", "Results", "ScanInput", "Scope"]
