#!/bin/sh
# %posttrans of the Electron shell's rpm (electron-builder: rpm.fpm ["--rpm-posttrans",
# "../../packaging/linux/rpm-posttrans.sh"]). It runs once the whole transaction is done.
#
# Why: 0.3.4's rpm (electron-builder's after-remove template) runs, in its %postun,
#     update-alternatives --remove spaceknife /opt/SpaceKnife/spaceknife
# on every removal, an upgrade included. On an upgrade rpm runs the new package's %post first and
# the old package's %postun after it, so that line takes away the /usr/bin/spaceknife link the new
# version has just registered: `spaceknife` (and `spaceknife --headless` in users' scripts and
# units) would be gone after updating from 0.3.4. Every later rpm carries the same %postun, so
# every later rpm carries this too. (packaging/update-from-034.mjs rpm checks it on Rocky Linux 9.)
if [ -x /opt/SpaceKnife/spaceknife ]; then
  if command -v update-alternatives >/dev/null 2>&1; then
    update-alternatives --install /usr/bin/spaceknife spaceknife /opt/SpaceKnife/spaceknife 100 \
      || ln -sf /opt/SpaceKnife/spaceknife /usr/bin/spaceknife
  else
    ln -sf /opt/SpaceKnife/spaceknife /usr/bin/spaceknife
  fi
fi
exit 0
