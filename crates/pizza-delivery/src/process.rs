use std::process::{Command, Stdio};

fn signal_group(signal: &str, pgid: u32) {
    let _ = Command::new("sh")
        .args([
            "-c",
            "kill -s \"$0\" -- \"$1\"",
            signal,
            &format!("-{pgid}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn terminate_group(pgid: u32) {
    signal_group("TERM", pgid);
}

pub fn kill_group(pgid: u32) {
    signal_group("KILL", pgid);
}

pub fn stop_leased_group(pid: u32, start: u64, grace: std::time::Duration) {
    if start_ticks(pid) != Some(start) {
        return;
    }
    terminate_group(pid);
    std::thread::spawn(move || {
        std::thread::sleep(grace);
        if start_ticks(pid) == Some(start) {
            kill_group(pid);
        }
    });
}

pub fn parse_start_ticks(stat: &str) -> Option<u64> {
    let after_name = stat.rsplit_once(')')?.1;
    after_name.split_whitespace().nth(19)?.parse().ok()
}

pub fn start_ticks(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| parse_start_ticks(&stat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_is_field_22_even_when_the_command_name_has_spaces_and_parens() {
        let stat = "123 (my (odd) cmd) S 1 123 123 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 200 18446744073709551615";
        assert_eq!(parse_start_ticks(stat), Some(987_654));
    }

    #[test]
    fn this_process_has_a_start_time() {
        assert!(start_ticks(std::process::id()).is_some());
    }
}
