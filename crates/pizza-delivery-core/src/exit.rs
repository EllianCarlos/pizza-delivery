#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success,
    General,
    Usage,
    NotFound,
    DaemonUnreachable,
    JobUnsuccessful,
}

impl ExitCode {
    pub fn code(self) -> u8 {
        match self {
            ExitCode::Success => 0,
            ExitCode::General => 1,
            ExitCode::Usage => 2,
            ExitCode::NotFound => 3,
            ExitCode::DaemonUnreachable => 4,
            ExitCode::JobUnsuccessful => 5,
        }
    }

    pub const HELP: &'static str = "Exit codes:\n  0  success\n  1  general error\n  2  usage error\n  3  job not found or id ambiguous\n  4  daemon unreachable after the start attempt\n  5  job ended unsuccessfully (--follow)";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_documented_contract() {
        let all = [
            (ExitCode::Success, 0),
            (ExitCode::General, 1),
            (ExitCode::Usage, 2),
            (ExitCode::NotFound, 3),
            (ExitCode::DaemonUnreachable, 4),
            (ExitCode::JobUnsuccessful, 5),
        ];
        for (code, n) in all {
            assert_eq!(code.code(), n);
            assert!(ExitCode::HELP.contains(&format!("  {n}  ")));
        }
    }
}
