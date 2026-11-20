//! Running the system tools discovery reads (`arp`, `route`, `tailscale`, `aws`, `wg`, `ping`)
//! with the login-shell PATH and a timeout. Nothing here writes anything.
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdError {
    NotFound,
    Timeout,
    Failed(String),
}

/// Run `program args…`, killing it after `timeout`.
pub fn run(program: &Path, args: &[&str], timeout: Duration) -> Result<Output, CmdError> {
    let mut c = Command::new(program);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in crate::shell_env::resolved() {
        c.env(k, v);
    }
    // never let a CLI wait for a pager or a prompt
    c.env("AWS_PAGER", "").env("PAGER", "cat");
    hide_console(&mut c);
    let mut child = c.spawn().map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { CmdError::NotFound } else { CmdError::Failed(e.to_string()) })?;
    let mut out = child.stdout.take().ok_or(CmdError::Failed(String::new()))?;
    let mut err = child.stderr.take().ok_or(CmdError::Failed(String::new()))?;
    let ro = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let re = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CmdError::Timeout);
            }
            Err(e) => return Err(CmdError::Failed(e.to_string())),
        }
    };
    let stdout = String::from_utf8_lossy(&ro.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&re.join().unwrap_or_default()).into_owned();
    Ok(Output { ok: status.success(), stdout, stderr })
}

/// [`run`] on a program looked up on the login-shell PATH.
pub fn run_named(name: &str, args: &[&str], timeout: Duration) -> Result<Output, CmdError> {
    let p = find(name, &[]).ok_or(CmdError::NotFound)?;
    run(&p, args, timeout)
}

/// `name` on the login-shell PATH, else the first of `extra` that exists.
pub fn find(name: &str, extra: &[&str]) -> Option<PathBuf> {
    crate::shell_env::which(name).or_else(|| extra.iter().map(PathBuf::from).find(|p| p.is_file())).or_else(|| {
        // system tools live in sbin on some systems
        ["/usr/sbin", "/sbin", "/usr/bin", "/bin"].iter().map(|d| Path::new(d).join(name)).find(|p| p.is_file())
    })
}

#[cfg(windows)]
fn hide_console(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}
