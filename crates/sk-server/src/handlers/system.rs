//! system.*, app.shutdown, `$/subscribe`, `$/unsubscribe`, the `log` topic (Python
//! `server/handlers.py`).

use std::time::Duration;

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

/// Windows "10" / "11", the kernel version elsewhere.
fn read_os_release() -> String {
    #[cfg(windows)]
    {
        // RtlGetVersion is the honest one; the version from the registry is good enough here
        if let Ok(out) = std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(v) = text.split("Version ").nth(1) {
                let v = v.trim().trim_end_matches(']');
                let mut parts = v.split('.');
                if let Some(major) = parts.next() {
                    let build = parts.nth(1).unwrap_or("");
                    return if major == "10" && build.parse::<u32>().is_ok_and(|b| b >= 22000) {
                        "11".to_owned()
                    } else {
                        major.to_owned()
                    };
                }
            }
        }
        String::new()
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
