"""SpaceKnife backend."""

import os

# numpy's bundled OpenBLAS starts a thread per core at import, each with its own buffer: about
# 750 MB of private commit per process on a 32-thread machine (the backend and every scan worker),
# for nothing — SpaceKnife never calls BLAS. Must run before numpy is first imported; spawned
# workers and the helper inherit the environment.
for _var in ("OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS", "MKL_NUM_THREADS"):
    os.environ.setdefault(_var, "1")

__version__ = "0.1.0"
