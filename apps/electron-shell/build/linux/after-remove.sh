#!/bin/bash
# The rpm's removal script (%postun): electron-builder's after-remove template (26.17) takes the
# /usr/bin links away whenever it runs, also when an upgrade replaced the package, and an upgrade
# runs the old package's %postun after the new one's %post: the new version would lose its link.
# rpm passes how many instances of the package remain: 0 when it is removed, 1 or more after an
# upgrade. Only a removal takes the links away. (0.3.4's own script does not ask: rpm-posttrans.sh
# puts the links back after such an upgrade.) electron-builder fills in the names in braces.

if [ -n "$1" ] && [ "$1" -ge 1 ] 2>/dev/null; then
    exit 0
fi

# Delete the link to the binary
if type update-alternatives >/dev/null 2>&1; then
    update-alternatives --remove '${executable}' '/opt/${sanitizedProductName}/${executable}'
else
    rm -f '/usr/bin/${executable}'
fi

# spaceknife-cli's link, when it is still ours
if [ "`readlink '/usr/bin/spaceknife-cli'`" = '/opt/${sanitizedProductName}/spaceknife-cli' ]; then
    rm -f '/usr/bin/spaceknife-cli'
fi
