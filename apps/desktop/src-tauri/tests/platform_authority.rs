use splice_shell_desktop_lib::platform::{
    PlatformErrorCode, PlatformFacts, PlatformServices, PlatformTarget, ShellCommand,
};

fn facts(
    os: &str,
    ubuntu: Option<&str>,
    wsl: Option<&str>,
    wslg: bool,
    path: Option<&str>,
) -> PlatformFacts {
    PlatformFacts {
        os: os.into(),
        ubuntu: ubuntu.map(Into::into),
        wsl: wsl.map(Into::into),
        wslg,
        path: path.map(Into::into),
    }
}

#[test]
fn platform_services_preserve_windows_and_match_linux_wsl_commands() {
    let known_path = std::env::temp_dir();
    let windows = PlatformServices::from_facts(facts(
        "windows",
        None,
        None,
        false,
        Some(r"C:\\Windows\\System32"),
    ))
    .unwrap();
    assert_eq!(windows.target(), PlatformTarget::Windows);
    assert_eq!(
        windows.shell().expect("Windows shell"),
        ShellCommand {
            program: "cmd.exe".into(),
            args: vec!["/D".into(), "/K".into()],
        }
    );
    assert_eq!(
        windows.reveal_command(&known_path).expect("Windows reveal"),
        ShellCommand {
            program: "explorer.exe".into(),
            args: vec![format!("/select,{}", known_path.display())],
        }
    );

    let ubuntu = PlatformServices::from_facts(facts(
        "linux",
        Some("24.04"),
        None,
        false,
        Some("/usr/bin:/bin"),
    ))
    .unwrap();
    assert_eq!(ubuntu.target(), PlatformTarget::NativeUbuntu);
    assert_eq!(
        ubuntu.shell().expect("Ubuntu shell"),
        ShellCommand {
            program: "/bin/sh".into(),
            args: vec![],
        }
    );

    let wsl = PlatformServices::from_facts(facts(
        "linux",
        None,
        Some("Ubuntu"),
        true,
        Some("/usr/bin:/bin"),
    ))
    .unwrap();
    assert_eq!(wsl.target(), PlatformTarget::Wsl2Wslg);
    assert_eq!(wsl.shell(), ubuntu.shell());
    assert_eq!(
        wsl.reveal_command(&known_path),
        ubuntu.reveal_command(&known_path)
    );
}

#[test]
fn platform_services_return_structured_errors_without_fallback() {
    let windows = PlatformServices::from_facts(facts(
        "windows",
        None,
        None,
        false,
        Some(r"C:\\Windows\\System32"),
    ))
    .unwrap();
    let wsl_without_wslg = PlatformServices::from_facts(facts(
        "linux",
        None,
        Some("Ubuntu"),
        false,
        Some("/usr/bin:/bin"),
    ))
    .unwrap_err();
    assert_eq!(wsl_without_wslg.code, PlatformErrorCode::WslgUnavailable);
    assert!(wsl_without_wslg.retryable);

    let unsupported =
        PlatformServices::from_facts(facts("macos", None, None, false, None)).unwrap_err();
    assert_eq!(unsupported.code, PlatformErrorCode::UnsupportedTarget);
    assert!(!unsupported.retryable);

    let relative_path = windows.reveal_command("relative/path").unwrap_err();
    assert_eq!(relative_path.code, PlatformErrorCode::InvalidPath);
    assert_eq!(relative_path.platform, Some(PlatformTarget::Windows));
}
