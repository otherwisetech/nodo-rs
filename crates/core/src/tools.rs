//! Ping and traceroute for the output panel: the system tools, their output line by line.
use std::io::{BufRead as _, BufReader};
use std::net::IpAddr;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ping,
    Traceroute,
}

/// Program and arguments for `tool` towards `ip` on this OS.
pub fn command(tool: Tool, ip: IpAddr) -> (String, Vec<String>) {
    let ip_s = ip.to_string();
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).chain(std::iter::once(ip_s.clone())).collect::<Vec<_>>();
    match (tool, cfg!(windows)) {
        (Tool::Ping, true) => ("ping".into(), v(&["-n", "5"])),
        (Tool::Ping, false) => ("ping".into(), v(&["-c", "5"])),
        (Tool::Traceroute, true) => ("tracert".into(), v(&["-d", "-h", "20"])),
        (Tool::Traceroute, false) if ip.is_ipv6() && cfg!(target_os = "macos") => ("traceroute6".into(), v(&["-n", "-w", "2", "-q", "1", "-m", "20"])),
        (Tool::Traceroute, false) => ("traceroute".into(), v(&["-n", "-w", "2", "-q", "1", "-m", "20"])),
    }
}

pub fn display(program: &str, args: &[String]) -> String {
    std::iter::once(program.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ")
}

/// A running tool: lines arrive on `lines` (stdout and stderr); the channel closes when it ends.
pub struct Running {
    pub lines: Receiver<String>,
    child: Arc<Mutex<Child>>,
}

impl Running {
    pub fn stop(&self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

pub fn spawn(tool: Tool, ip: IpAddr) -> std::io::Result<Running> {
    let (program, args) = command(tool, ip);
    let path = crate::cmd::find(&program, &[]).unwrap_or_else(|| program.clone().into());
    let mut c = Command::new(path);
    c.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in crate::shell_env::resolved() {
        c.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let mut child = c.spawn()?;
    let (tx, rx) = channel();
    let out = child.stdout.take();
    let err = child.stderr.take();
    let tx2 = tx.clone();
    if let Some(o) = out {
        std::thread::spawn(move || {
            for l in BufReader::new(o).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(e) = err {
        std::thread::spawn(move || {
            for l in BufReader::new(e).lines().map_while(Result::ok) {
                if tx2.send(l).is_err() {
                    break;
                }
            }
        });
    }
    let child = Arc::new(Mutex::new(child));
    let reaper = child.clone();
    std::thread::spawn(move || {
        // poll so stop() can still take the lock to kill it
        loop {
            if let Ok(Some(_)) = reaper.lock().unwrap().try_wait() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    Ok(Running { lines: rx, child })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        let ip: IpAddr = "192.168.1.1".parse().unwrap();
        let (p, a) = command(Tool::Ping, ip);
        assert_eq!(p, "ping");
        assert_eq!(a.last().map(String::as_str), Some("192.168.1.1"));
        let (p, _) = command(Tool::Traceroute, ip);
        assert!(p == "traceroute" || p == "tracert");
        assert!(display("ping", &a).starts_with("ping -"));
    }
}
