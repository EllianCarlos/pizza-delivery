use std::path::Path;
use std::process::{Command, Output};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn bin(home: &Path, args: &[&str]) -> TestResult<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_pizza-delivery"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .output()?)
}

#[test]
fn completions_are_generated_for_every_supported_shell() -> TestResult {
    let home = tempfile::tempdir()?;
    for (shell, marker) in [
        ("bash", "pizza-delivery"),
        ("zsh", "#compdef pizza-delivery"),
        ("fish", "complete -c pizza-delivery"),
    ] {
        let out = bin(home.path(), &["completions", shell])?;
        assert!(out.status.success(), "{shell}: {:?}", out.stderr);
        let text = String::from_utf8(out.stdout)?;
        assert!(text.contains(marker), "{shell} missing {marker}");
        assert!(
            text.contains("install-service"),
            "{shell} lacks subcommands"
        );
    }
    Ok(())
}

#[test]
fn install_service_writes_a_socket_and_service_user_unit_pair() -> TestResult {
    let home = tempfile::tempdir()?;
    let out = bin(home.path(), &["install-service"])?;
    assert!(out.status.success(), "{:?}", out.stderr);
    let units = home.path().join("config/systemd/user");
    let socket = std::fs::read_to_string(units.join("pizza-delivery.socket"))?;
    let service = std::fs::read_to_string(units.join("pizza-delivery.service"))?;
    assert!(socket.contains("ListenStream=%t/pizza-delivery.sock"));
    assert!(socket.contains("WantedBy=sockets.target"));
    let exe = env!("CARGO_BIN_EXE_pizza-delivery");
    assert!(service.contains(&format!("ExecStart={exe} daemon")));
    assert!(String::from_utf8(out.stdout)?
        .contains("systemctl --user enable --now pizza-delivery.socket"));
    Ok(())
}

#[test]
fn install_service_is_idempotent() -> TestResult {
    let home = tempfile::tempdir()?;
    for _ in 0..2 {
        assert!(bin(home.path(), &["install-service"])?.status.success());
    }
    assert!(home
        .path()
        .join("config/systemd/user/pizza-delivery.socket")
        .is_file());
    Ok(())
}
