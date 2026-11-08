//! The update's swap helper letting go of its standard descriptors (`update::swap::point_at_null`).
//! Alone in a test binary of its own: it counts this process's open descriptors.
#![cfg(target_os = "linux")]

use std::path::Path;

fn open_descriptors() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

#[test]
fn the_descriptor_opened_for_dev_null_does_not_stay_open() {
    let mut fds = [0i32; 2];
    // SAFETY: a fresh pipe; both ends are closed below.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let before = open_descriptors();
    sk_core::update::swap::point_at_null(&fds);
    for fd in fds {
        let target = std::fs::read_link(format!("/proc/self/fd/{fd}")).unwrap();
        assert_eq!(target, Path::new("/dev/null"), "descriptor {fd}");
    }
    assert_eq!(open_descriptors(), before, "nothing else is left open");
    for fd in fds {
        // SAFETY: ours, closed once.
        unsafe { libc::close(fd) };
    }
}
