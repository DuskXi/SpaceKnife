//! system.*, app.shutdown, `$/subscribe`, `$/unsubscribe`, the `log` topic (Python
//! `server/handlers.py`).

use std::time::Duration;

use sk_protocol::io::TOPIC_IO;
use sk_protocol::methods::{AppShutdown, Subscribe, SystemInfoMethod, SystemPing, Unsubscribe};
use sk_protocol::streams::{SubscribeResult, TOPIC_LOG};
use sk_protocol::system::{PingResult, PlatformInfo, ShutdownResult, SystemInfo};
use sk_protocol::{CAP_LOG_STREAM, CAP_SUBSCRIPTIONS, EmptyResult, ErrorCode};

use crate::rpc::{Router, RpcError};
use crate::topics::{TopicRegistry, open_log};

/// The OS release (`platform.release()`), asked once.
fn os_release() -> String {
    static RELEASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RELEASE.get_or_init(read_os_release).clone()
}

/// Windows: "10", "11", "2022Server" … as 0.3.4 said ([`windows_release`] of the version
/// RtlGetVersion reports: the system's own, not the one a manifest or compatibility mode makes
/// GetVersionEx tell, and not `cmd /C ver`'s text, which depends on the console's language and is
/// empty when a Git for Windows / MSYS2 terminal started the app). Elsewhere the kernel's release.
fn read_os_release() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
        use windows_sys::Win32::System::SystemInformation::{OSVERSIONINFOEXW, OSVERSIONINFOW};
        const VER_NT_WORKSTATION: u8 = 1;
        let mut info = OSVERSIONINFOEXW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
            ..Default::default()
        };
        // SAFETY: an OSVERSIONINFOEXW whose size field says so, which RtlGetVersion fills in.
        let status = unsafe { RtlGetVersion((&raw mut info).cast::<OSVERSIONINFOW>()) };
        if status != 0 {
            return String::new();
        }
        windows_release(
            (info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber),
            info.wProductType == VER_NT_WORKSTATION,
        )
        .to_owned()
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    }
}

/// Python 3.12's `platform.release()` (0.3.4's) for Windows `(major, minor, build)`: the first
/// release of its table (workstations or servers) at or below the version; "" below them all.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_release(version: (u32, u32, u32), workstation: bool) -> &'static str {
    // platform._WIN32_CLIENT_RELEASES and _WIN32_SERVER_RELEASES, newest first
    const CLIENT: &[((u32, u32, u32), &str)] = &[
        ((10, 1, 0), "post11"),
        ((10, 0, 22000), "11"),
        ((6, 4, 0), "10"),
        ((6, 3, 0), "8.1"),
        ((6, 2, 0), "8"),
        ((6, 1, 0), "7"),
        ((6, 0, 0), "Vista"),
        ((5, 2, 3790), "XP64"),
        ((5, 2, 0), "XPMedia"),
        ((5, 1, 0), "XP"),
        ((5, 0, 0), "2000"),
    ];
    const SERVER: &[((u32, u32, u32), &str)] = &[
        ((10, 1, 0), "post2022Server"),
        ((10, 0, 20348), "2022Server"),
        ((10, 0, 17763), "2019Server"),
        ((6, 4, 0), "2016Server"),
        ((6, 3, 0), "2012ServerR2"),
        ((6, 2, 0), "2012Server"),
        ((6, 1, 0), "2008ServerR2"),
        ((6, 0, 0), "2008Server"),
        ((5, 2, 0), "2003Server"),
        ((5, 0, 0), "2000Server"),
    ];
    let table = if workstation { CLIENT } else { SERVER };
    table
        .iter()
        .find(|(v, _)| *v <= version)
        .map_or("", |(_, r)| *r)
}

fn os_system() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Darwin"
    } else {
        "Linux"
    }
}

fn machine() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => {
            if cfg!(windows) {
                "AMD64"
            } else {
                "x86_64"
            }
        }
        "aarch64" => {
            if cfg!(windows) {
                "ARM64"
            } else if cfg!(target_os = "macos") {
                "arm64"
            } else {
                "aarch64"
            }
        }
        other => other,
    }
}

pub fn install(r: &mut Router, t: &mut TopicRegistry) {
    t.register(TOPIC_LOG, |server, session, params| {
        open_log(server.log_bus.clone(), session, params)
    });

    r.add::<SystemInfoMethod, _, _>(|call, _| async move {
        let server = &call.server;
        let (release, fda) =
            crate::compute::blocking(|| (os_release(), sk_core::privacy::full_disk_access(None)))
                .await?;
        Ok(SystemInfo {
            server: server.server_info.clone(),
            pid: i64::from(std::process::id()),
            started_at: server.started_at,
            platform: PlatformInfo {
                system: os_system().to_owned(),
                release,
                machine: machine().to_owned(),
            },
            sessions: server.sessions().len() as i64,
            full_disk_access: fda,
        })
    });

    r.add::<SystemPing, _, _>(|_, p| async move {
        Ok(PingResult {
            payload: p.payload,
            server_time: chrono::Utc::now(),
        })
    });

    r.add::<AppShutdown, _, _>(|call, _| async move {
        // answer first, then stop: the response must reach the client before sockets close
        let server = call.server.clone();
        call.server.rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            server.request_shutdown();
        });
        Ok(ShutdownResult { accepted: true })
    });

    r.add::<Subscribe, _, _>(|call, p| async move {
        let s = &call.session;
        if !s.has_cap(CAP_SUBSCRIPTIONS) {
            return Err(RpcError::invalid_request(
                "subscriptions capability was not negotiated",
            ));
        }
        if p.topic == TOPIC_LOG && !s.has_cap(CAP_LOG_STREAM) {
            return Err(RpcError::invalid_request(
                "log-stream capability was not negotiated",
            ));
        }
        if p.topic == TOPIC_IO {
            // the `io` topic follows the disks the registry knows when subscribing: right after
            // the backend's start, those it is still finding (0.3.4 had them by the first session)
            call.server.scans.io_started().await;
        }
        let sub = call.server.topics.open(&call.server, s, &p)?;
        let id = sub.id.clone();
        s.add_subscription(sub);
        Ok(SubscribeResult { sub: id })
    });

    r.add::<Unsubscribe, _, _>(|call, p| async move {
        if !call.session.remove_subscription(&p.sub) {
            return Err(RpcError::new(
                ErrorCode::UnknownSubscription,
                format!("unknown subscription: {}", p.sub),
            ));
        }
        Ok(EmptyResult {})
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the registry says this Windows is (`CurrentMajorVersionNumber`, `CurrentBuildNumber`,
    /// `InstallationType`), read with reg.exe: an oracle that is not the code under test.
    #[cfg(windows)]
    fn registry() -> (u32, u32, bool) {
        let value = |name: &str| {
            let out = std::process::Command::new("reg")
                .args([
                    "query",
                    r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                    "/v",
                    name,
                ])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let line = text
                .lines()
                .find(|l| l.trim_start().starts_with(name))
                .unwrap_or("");
            line.split_whitespace().last().unwrap_or("").to_owned()
        };
        let major = value("CurrentMajorVersionNumber");
        let major = u32::from_str_radix(major.trim_start_matches("0x"), 16).unwrap();
        let build = value("CurrentBuildNumber").parse().unwrap();
        (major, build, value("InstallationType") != "Server")
    }

    /// `platform.release()` of 0.3.4 (Python 3.12) on this computer, whatever started it (a
    /// terminal of Git for Windows or MSYS2, a console in another language).
    #[cfg(windows)]
    #[test]
    fn the_release_is_the_systems_own() {
        let (major, build, client) = registry();
        let want = match (client, major, build) {
            (true, 10, b) if b >= 22000 => "11",
            (true, 10, _) => "10",
            (false, 10, b) if b >= 20348 => "2022Server",
            (false, 10, b) if b >= 17763 => "2019Server",
            (false, 10, _) => "2016Server",
            _ => panic!("not a Windows 10 or later: {major}.{build}"),
        };
        assert_eq!(read_os_release(), want);
    }

    /// The releases Python 3.12 names (platform._WIN32_CLIENT_RELEASES / _SERVER_RELEASES).
    #[test]
    fn windows_releases_as_python_names_them() {
        let client = |v| windows_release(v, true);
        let server = |v| windows_release(v, false);
        assert_eq!(client((10, 0, 26300)), "11");
        assert_eq!(client((10, 0, 22000)), "11");
        assert_eq!(client((10, 0, 21999)), "10");
        assert_eq!(client((10, 0, 19045)), "10");
        assert_eq!(client((6, 3, 9600)), "8.1");
        assert_eq!(client((6, 1, 7601)), "7");
        assert_eq!(client((5, 2, 3790)), "XP64");
        assert_eq!(client((10, 1, 0)), "post11");
        assert_eq!(client((4, 0, 1381)), "");
        assert_eq!(server((10, 0, 26100)), "2022Server"); // Server 2025: Python 3.12 has no name
        assert_eq!(server((10, 0, 20348)), "2022Server");
        assert_eq!(server((10, 0, 17763)), "2019Server");
        assert_eq!(server((10, 0, 14393)), "2016Server");
        assert_eq!(server((6, 3, 9600)), "2012ServerR2");
    }
}
