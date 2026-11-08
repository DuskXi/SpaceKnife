#!/bin/bash
# The rpm's %posttrans: after the whole transaction. An upgrade from 0.3.4 runs 0.3.4's removal
# script after this package's install script, and that one removes the spaceknife alternative and
# with it /usr/bin/spaceknife; put the links back when they are missing. (Not a template: fpm takes
# this file as it is.)

if [ -x /opt/SpaceKnife/spaceknife ] && [ ! -e /usr/bin/spaceknife ]; then
    if type update-alternatives >/dev/null 2>&1; then
        update-alternatives --install /usr/bin/spaceknife spaceknife /opt/SpaceKnife/spaceknife 100 || ln -sf /opt/SpaceKnife/spaceknife /usr/bin/spaceknife
    else
        ln -sf /opt/SpaceKnife/spaceknife /usr/bin/spaceknife
    fi
fi
if [ -x /opt/SpaceKnife/spaceknife-cli ] && [ ! -e /usr/bin/spaceknife-cli ]; then
    ln -sf /opt/SpaceKnife/spaceknife-cli /usr/bin/spaceknife-cli
fi
exit 0
