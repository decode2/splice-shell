mod wsl_runtime_support;

use std::{fs, path::PathBuf};

use wsl_runtime_support::{
    classify, require_authoritative_receipt, run_receipt, write_receipt, WslFacts, WslRuntime,
};

#[test]
fn deterministic_fixtures_distinguish_ready_wslg_from_named_skips() {
    let ready = classify(WslFacts {
        os_release: "ID=ubuntu\nVERSION_ID=\"24.04\"\n",
        kernel_release: "6.8.0-microsoft-standard-WSL2",
        distro_name: Some("Ubuntu"),
        wayland_display: Some("wayland-0"),
        wayland_socket: true,
    });
    assert_eq!(ready, WslRuntime::Ready);

    let generic_linux = classify(WslFacts {
        kernel_release: "6.8.0-generic",
        ..WslFacts::supported()
    });
    assert_eq!(
        generic_linux,
        WslRuntime::Skip("not a WSL2 kernel; generic Linux must not claim a WSL receipt")
    );

    let missing_wslg = classify(WslFacts {
        wayland_display: None,
        wayland_socket: false,
        ..WslFacts::supported()
    });
    assert_eq!(
        missing_wslg,
        WslRuntime::Skip("WSLg Wayland display is unavailable")
    );
}

#[test]
fn required_mode_rejects_a_skipped_receipt() {
    let skipped = WslRuntime::Skip("deterministic unsupported fixture");
    assert!(require_authoritative_receipt(skipped, false).is_ok());
    assert_eq!(
        require_authoritative_receipt(skipped, true),
        Err("deterministic unsupported fixture")
    );
}

#[test]
fn guarded_wsl_receipt_workflow_requires_a_real_self_hosted_wslg_host() {
    let workflow = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let workflow = fs::read_to_string(workflow).expect("CI workflow is readable");

    for required_contract in [
        "run_wslg_receipt",
        "WSL2/WSLg runtime receipt (manual, authoritative)",
        "runs-on: [self-hosted, linux, x64, wsl2, wslg]",
        "SPLICE_WSL_RECEIPT: artifacts/wsl-runtime-receipt.json",
        "SPLICE_WSL_RECEIPT_REQUIRED: 'true'",
        "grep -q '^ID=ubuntu$' /etc/os-release",
        "grep -Eq '^VERSION_ID=\"(22.04|24.04)\"$' /etc/os-release",
        "cargo test -p splice-pty --test wsl_runtime_receipt -- --nocapture",
        "cargo test -p splice-shell-desktop --test platform_authority",
        "actions/upload-artifact",
        "Successful real WSL2/WSLg receipt is required before support may be declared.",
    ] {
        assert!(
            workflow.contains(required_contract),
            "WSL receipt workflow must declare: {required_contract}"
        );
    }

    let actionlint =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/actionlint.yaml");
    let actionlint = fs::read_to_string(actionlint).expect("actionlint custom-runner metadata");
    assert!(actionlint.contains("- wsl2") && actionlint.contains("- wslg"));
}

#[test]
fn runtime_harness_emits_a_named_skip_or_a_real_wslg_receipt() {
    let status = run_receipt();
    write_receipt(status);
    match status {
        WslRuntime::Ready => println!("RECEIPT: WSL2/WSLg PTY runtime passed"),
        WslRuntime::Skip(reason) => println!("SKIP: WSL2/WSLg PTY runtime: {reason}"),
    }
    let required = std::env::var("SPLICE_WSL_RECEIPT_REQUIRED").is_ok_and(|value| value == "true");
    require_authoritative_receipt(status, required)
        .expect("authoritative WSL2/WSLg receipt must not skip");
}
