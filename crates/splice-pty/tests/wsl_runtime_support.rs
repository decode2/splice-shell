use std::{
    fs,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use splice_pty::{PtySession, TerminalSize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WslRuntime {
    Ready,
    Skip(&'static str),
}

pub struct WslFacts<'a> {
    pub os_release: &'a str,
    pub kernel_release: &'a str,
    pub distro_name: Option<&'a str>,
    pub wayland_display: Option<&'a str>,
    pub wayland_socket: bool,
}

impl<'a> WslFacts<'a> {
    pub fn supported() -> Self {
        Self {
            os_release: "ID=ubuntu\nVERSION_ID=\"24.04\"\n",
            kernel_release: "6.8.0-microsoft-standard-WSL2",
            distro_name: Some("Ubuntu"),
            wayland_display: Some("wayland-0"),
            wayland_socket: true,
        }
    }
}

pub fn classify(facts: WslFacts<'_>) -> WslRuntime {
    if !facts.kernel_release.to_ascii_lowercase().contains("wsl2") {
        return WslRuntime::Skip("not a WSL2 kernel; generic Linux must not claim a WSL receipt");
    }
    if facts.distro_name.is_none_or(str::is_empty) {
        return WslRuntime::Skip("WSL_DISTRO_NAME is required for an in-distro WSL receipt");
    }
    if !facts.os_release.lines().any(|line| line == "ID=ubuntu")
        || !facts
            .os_release
            .lines()
            .any(|line| matches!(line, "VERSION_ID=\"22.04\"" | "VERSION_ID=\"24.04\""))
    {
        return WslRuntime::Skip("WSL receipt requires Ubuntu 22.04 or 24.04");
    }
    if facts.wayland_display.is_none_or(str::is_empty) || !facts.wayland_socket {
        return WslRuntime::Skip("WSLg Wayland display is unavailable");
    }
    WslRuntime::Ready
}

pub fn require_authoritative_receipt(
    status: WslRuntime,
    required: bool,
) -> Result<(), &'static str> {
    match (required, status) {
        (true, WslRuntime::Skip(reason)) => Err(reason),
        _ => Ok(()),
    }
}

fn receive_until(receiver: &mpsc::Receiver<String>, marker: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = String::new();
    while Instant::now() < deadline && !output.contains(marker) {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.push_str(&chunk);
        }
    }
    output
}

fn is_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn is_wayland_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;

    fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(not(unix))]
fn is_wayland_socket(_: &Path) -> bool {
    false
}

fn skip_or_ready() -> WslRuntime {
    let kernel_release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let os_release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").ok();
    let socket = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .zip(display.as_deref())
        .is_some_and(|(runtime_dir, display)| {
            is_wayland_socket(&Path::new(&runtime_dir).join(display))
        });
    classify(WslFacts {
        os_release: &os_release,
        kernel_release: &kernel_release,
        distro_name: std::env::var("WSL_DISTRO_NAME").ok().as_deref(),
        wayland_display: display.as_deref(),
        wayland_socket: socket,
    })
}

pub fn run_receipt() -> WslRuntime {
    let status = skip_or_ready();
    if status != WslRuntime::Ready {
        return status;
    }
    if !Path::new("/bin/sh").is_file() {
        return WslRuntime::Skip("WSL default shell prerequisite /bin/sh is unavailable");
    }
    if std::env::var_os("PATH").is_none() {
        return WslRuntime::Skip("PATH is required for WSL platform authority and path reveal");
    }
    if !matches!(std::process::Command::new("sh")
        .args(["-c", "command -v xdg-open"])
        .status(), Ok(status) if status.success())
    {
        return WslRuntime::Skip("xdg-open is required for WSL path reveal");
    }

    let size = TerminalSize::new(80, 24).expect("fixture terminal size");
    let (sender, receiver) = mpsc::channel();
    let session = PtySession::spawn(
        "/bin/sh",
        &["-c", "printf 'ready utf8=café\\n'; stty size; read value; printf 'input=%s\\n' \"$value\"; stty size"],
        size,
        move |_, output| { let _ = sender.send(output); },
        |_| {},
    ).expect("WSL default shell starts");
    assert!(session.is_running().expect("liveness is available"));
    assert!(receive_until(&receiver, "24 80").contains("utf8=café"));
    session
        .resize(TerminalSize::new(132, 43).expect("resize size"))
        .expect("resize succeeds");
    session.write("wsl-input\n").expect("input succeeds");
    assert!(receive_until(&receiver, "43 132").contains("input=wsl-input"));
    session.close();
    assert!(!session.is_running().expect("liveness after close"));

    let (sender, receiver) = mpsc::channel();
    let session = PtySession::spawn(
        "/bin/sh",
        &[
            "-c",
            "trap 'printf interrupted; exit' INT; printf ready; while :; do sleep 1; done",
        ],
        size,
        move |_, output| {
            let _ = sender.send(output);
        },
        |_| {},
    )
    .expect("WSL interrupt shell starts");
    assert!(receive_until(&receiver, "ready").contains("ready"));
    session.interrupt().expect("interrupt succeeds");
    assert!(receive_until(&receiver, "interrupted").contains("interrupted"));
    session.close();

    let (sender, receiver) = mpsc::channel();
    let session = PtySession::spawn(
        "/bin/sh",
        &["-c", "trap '' HUP TERM; sh -c 'trap \"\" HUP TERM; echo child=$$; while :; do sleep 1; done' & wait"],
        size,
        move |_, output| { let _ = sender.send(output); },
        |_| {},
    ).expect("WSL teardown shell starts");
    let output = receive_until(&receiver, "child=");
    let child = output
        .split("child=")
        .nth(1)
        .expect("child pid")
        .lines()
        .next()
        .expect("child pid line")
        .trim();
    session.close();
    session.close();
    assert!(
        !is_alive(child),
        "close must tear down the WSL process group"
    );
    WslRuntime::Ready
}

pub fn write_receipt(status: WslRuntime) {
    let (outcome, reason) = match status {
        WslRuntime::Ready => ("passed", "all WSL2/WSLg PTY probes passed"),
        WslRuntime::Skip(reason) => ("skipped", reason),
    };
    let receipt = format!(
        "{{\"target\":\"linux-native-wsl2-wslg\",\"status\":\"{outcome}\",\"reason\":\"{reason}\",\"probes\":[\"startup\",\"utf8\",\"input\",\"resize\",\"interrupt\",\"process_group_teardown\",\"liveness\",\"close\"],\"platform_prerequisites\":[\"platform_authority\",\"default_shell\",\"PATH\",\"xdg-open\"]}}"
    );
    if let Some(path) = std::env::var_os("SPLICE_WSL_RECEIPT") {
        let path = Path::new(&path);
        fs::create_dir_all(path.parent().expect("receipt has a parent"))
            .expect("receipt directory");
        fs::write(path, &receipt).expect("machine-readable WSL receipt");
    }
    println!("WSL_RECEIPT: {receipt}");
}
