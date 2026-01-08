use base64::Engine as _;
use intar_probes::{ActionEvent, ProbeResult, Request, Response, SshSessionKind, evaluate_probe};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::openpty;
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{close, dup2_stderr, dup2_stdin, dup2_stdout, read, setsid, write};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, BorrowedFd, IntoRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VIRTIO_AGENT_PORT: &str = "/dev/virtio-ports/intar.agent";
const FALLBACK_AGENT_PORT: &str = "/dev/vport0p1";
const VIRTIO_ACTIONS_PORT: &str = "/dev/virtio-ports/intar.actions";
const ACTIONS_SOCK_PATH: &str = "/run/intar/actions.sock";

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("record-ssh") => {
            let real_shell = args.next().unwrap_or_else(|| "/bin/bash".into());
            let exit_code = record_ssh(&real_shell).unwrap_or(1);
            std::process::exit(exit_code);
        }
        Some("record-command") => {
            let real_shell = args.next().unwrap_or_else(|| "/bin/bash".into());
            let command = args.next().unwrap_or_default();
            let exit_code = record_command(&real_shell, &command).unwrap_or(1);
            std::process::exit(exit_code);
        }
        _ => daemon(),
    }
}

fn daemon() {
    let start_time = Instant::now();

    eprintln!("intar-agent starting...");

    std::thread::spawn(|| {
        loop {
            if let Err(e) = run_actions_sink() {
                eprintln!("actions sink error: {e}; retrying in 1s...");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });

    let port_path = if std::path::Path::new(VIRTIO_AGENT_PORT).exists() {
        VIRTIO_AGENT_PORT
    } else if std::path::Path::new(FALLBACK_AGENT_PORT).exists() {
        FALLBACK_AGENT_PORT
    } else {
        eprintln!("No virtio-serial probe port found, exiting");
        std::process::exit(1);
    };

    eprintln!("Using virtio-serial probe port: {port_path}");

    loop {
        match run_probe_agent(port_path, &start_time) {
            Ok(()) => {
                eprintln!("Probe agent loop ended, restarting...");
            }
            Err(e) => {
                eprintln!("Probe agent error: {e}, retrying in 1s...");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn run_actions_sink() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = std::path::Path::new(ACTIONS_SOCK_PATH).parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::path::Path::new(ACTIONS_SOCK_PATH).exists() {
        let _ = std::fs::remove_file(ACTIONS_SOCK_PATH);
    }

    let listener = UnixListener::bind(ACTIONS_SOCK_PATH)?;
    std::fs::set_permissions(ACTIONS_SOCK_PATH, std::fs::Permissions::from_mode(0o666))?;

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || actions_writer_loop(rx));

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream);
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let trimmed = line.trim_end();
                                if trimmed.is_empty() {
                                    continue;
                                }
                                let _ = tx.send(trimmed.to_string());
                            }
                        }
                    }
                });
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

fn actions_writer_loop(rx: std::sync::mpsc::Receiver<String>) {
    let mut port: Option<File> = None;

    for line in rx {
        loop {
            if port.is_none() {
                if let Ok(f) = File::options().write(true).open(VIRTIO_ACTIONS_PORT) {
                    port = Some(f);
                } else {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            }

            let Some(f) = port.as_mut() else {
                continue;
            };

            if writeln!(f, "{line}").is_ok() && f.flush().is_ok() {
                break;
            }

            port = None;
        }
    }
}

fn run_probe_agent(
    port_path: &str,
    start_time: &Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let port = File::options().read(true).write(true).open(port_path)?;

    let mut writer = port.try_clone()?;
    let mut reader = BufReader::new(port);

    eprintln!("Connected to virtio-serial probe port");

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let response = match serde_json::from_str::<Request>(line) {
                    Ok(request) => handle_probe_request(request, start_time),
                    Err(e) => Response::Error {
                        message: format!("Failed to parse request: {e}"),
                    },
                };

                let response_json = serde_json::to_string(&response)?;
                writeln!(writer, "{response_json}")?;
                writer.flush()?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn handle_probe_request(request: Request, start_time: &Instant) -> Response {
    match request {
        Request::Ping => Response::Pong {
            uptime_secs: start_time.elapsed().as_secs(),
        },
        Request::CheckProbe { id, spec } => {
            let result = evaluate_probe(&id, &spec);
            Response::ProbeResult {
                id: result.id,
                passed: result.passed,
                message: result.message,
            }
        }
        Request::CheckAll { probes } => {
            let results: Vec<ProbeResult> = probes
                .into_iter()
                .map(|(id, spec)| evaluate_probe(&id, &spec))
                .collect();
            Response::AllResults { results }
        }
    }
}

fn record_command(real_shell: &str, command: &str) -> Result<i32, Box<dyn std::error::Error>> {
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let mut sink = connect_actions_sink();
    if let Some(s) = sink.as_mut() {
        send_event(
            s,
            &ActionEvent::SshSessionStart {
                ts_unix_ms: unix_ms(),
                user,
                kind: SshSessionKind::Command,
            },
        );
        if !command.is_empty() {
            send_event(
                s,
                &ActionEvent::SshLine {
                    ts_unix_ms: unix_ms(),
                    line: command.to_string(),
                },
            );
        }
    }

    let output = Command::new(real_shell).args(["-c", command]).output()?;
    let code = output.status.code().unwrap_or(1);

    if let Some(s) = sink.as_mut() {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if line.trim().is_empty() {
                continue;
            }
            send_event(
                s,
                &ActionEvent::SshOutput {
                    ts_unix_ms: unix_ms(),
                    line: line.to_string(),
                },
            );
        }
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            if line.trim().is_empty() {
                continue;
            }
            send_event(
                s,
                &ActionEvent::SshOutput {
                    ts_unix_ms: unix_ms(),
                    line: line.to_string(),
                },
            );
        }
        send_event(
            s,
            &ActionEvent::SshSessionEnd {
                ts_unix_ms: unix_ms(),
                exit_code: code,
            },
        );
    }

    Ok(code)
}

fn record_ssh(real_shell: &str) -> Result<i32, Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if !is_tty(stdin.as_raw_fd()) || !is_tty(stdout.as_raw_fd()) {
        return Ok(1);
    }

    let _raw_mode = enable_raw_mode(&stdin);

    let (master_fd, slave_fd) = open_pty()?;

    let mut child = spawn_shell_in_pty(real_shell, slave_fd, master_fd)?;

    close(slave_fd).ok();

    let (tx, writer_thread) = start_actions_event_stream();
    send_session_start(&tx);

    let proxy_result = proxy_pty_session(master_fd, &tx);
    if proxy_result.is_err() {
        let _ = child.kill();
    }
    close(master_fd).ok();

    let status = wait_for_child_exit(&mut child)?;
    let code = status.code().unwrap_or(1);

    send_session_end(&tx, code);
    drop(tx);

    let _ = writer_thread.join();

    proxy_result?;
    Ok(code)
}

struct RawModeGuard<'a> {
    stdin: &'a std::io::Stdin,
    orig: Termios,
}

impl Drop for RawModeGuard<'_> {
    fn drop(&mut self) {
        let _ = tcsetattr(self.stdin, SetArg::TCSANOW, &self.orig);
    }
}

fn enable_raw_mode(stdin: &std::io::Stdin) -> Option<RawModeGuard<'_>> {
    let orig = tcgetattr(stdin).ok()?;
    let mut raw = orig.clone();
    cfmakeraw(&mut raw);
    tcsetattr(stdin, SetArg::TCSANOW, &raw).ok()?;
    Some(RawModeGuard { stdin, orig })
}

fn open_pty() -> Result<(RawFd, RawFd), Box<dyn std::error::Error>> {
    let pty = openpty(None, None)?;
    Ok((pty.master.into_raw_fd(), pty.slave.into_raw_fd()))
}

fn spawn_shell_in_pty(
    real_shell: &str,
    slave_fd: RawFd,
    master_fd: RawFd,
) -> Result<std::process::Child, Box<dyn std::error::Error>> {
    let mut cmd = Command::new(real_shell);
    cmd.arg("-l");
    unsafe {
        cmd.pre_exec(move || {
            setsid().map_err(to_io_err)?;
            if nix::libc::ioctl(slave_fd, nix::libc::TIOCSCTTY.into(), 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let slave_borrowed = BorrowedFd::borrow_raw(slave_fd);
            dup2_stdin(slave_borrowed).map_err(to_io_err)?;
            dup2_stdout(slave_borrowed).map_err(to_io_err)?;
            dup2_stderr(slave_borrowed).map_err(to_io_err)?;
            close(master_fd).ok();
            close(slave_fd).ok();
            Ok(())
        });
    }
    Ok(cmd.spawn()?)
}

fn proxy_pty_session(
    master_fd: RawFd,
    tx: &std::sync::mpsc::Sender<ActionEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = std::io::stdout();
    let stdin_fd = std::io::stdin().as_raw_fd();
    let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
    let master_borrowed = unsafe { BorrowedFd::borrow_raw(master_fd) };

    let mut buf = [0u8; 4096];
    let mut line = String::new();
    let mut input_escape = false;
    let mut output_line = String::new();
    let mut output_escape = false;

    loop {
        let mut fds = [
            PollFd::new(stdin_borrowed, PollFlags::POLLIN),
            PollFd::new(master_borrowed, PollFlags::POLLIN),
        ];

        match poll(&mut fds, PollTimeout::NONE) {
            Ok(0) | Err(Errno::EINTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        if is_fd_readable(&fds[1]) {
            match read(master_borrowed, &mut buf) {
                Ok(0) | Err(Errno::EIO) => break,
                Ok(n) => {
                    stdout.write_all(&buf[..n])?;
                    stdout.flush()?;
                    derive_lines_from_output(&buf[..n], &mut output_line, &mut output_escape, tx);
                }
                Err(Errno::EINTR) => {}
                Err(e) => return Err(e.into()),
            }
        }

        if is_fd_readable(&fds[0]) {
            match read(stdin_borrowed, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    write_all_fd(master_borrowed, chunk)?;

                    let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
                    let _ = tx.send(ActionEvent::SshRawInput {
                        ts_unix_ms: unix_ms(),
                        data_b64: b64,
                    });

                    derive_lines_from_input(chunk, &mut line, &mut input_escape, tx);
                }
                Err(Errno::EINTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    let trimmed = output_line.trim();
    if !trimmed.is_empty() && !is_prompt_line(trimmed) {
        let _ = tx.send(ActionEvent::SshOutput {
            ts_unix_ms: unix_ms(),
            line: trimmed.to_string(),
        });
    }

    Ok(())
}

fn is_fd_readable(fd: &PollFd<'_>) -> bool {
    let revents = fd.revents().unwrap_or(PollFlags::empty());
    revents.contains(PollFlags::POLLIN)
        || revents.contains(PollFlags::POLLHUP)
        || revents.contains(PollFlags::POLLERR)
}

fn derive_lines_from_input(
    chunk: &[u8],
    line: &mut String,
    in_escape: &mut bool,
    tx: &std::sync::mpsc::Sender<ActionEvent>,
) {
    for &b in chunk {
        if *in_escape {
            if (b as char).is_ascii_alphabetic() || b == b'~' {
                *in_escape = false;
            }
            continue;
        }

        match b {
            0x1b => {
                *in_escape = true;
            }
            b'\r' | b'\n' => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let _ = tx.send(ActionEvent::SshLine {
                        ts_unix_ms: unix_ms(),
                        line: trimmed.to_string(),
                    });
                }
                line.clear();
            }
            0x7f | 0x08 => {
                let _ = line.pop();
            }
            b'\t' => line.push('\t'),
            b if b.is_ascii_graphic() || b == b' ' => line.push(char::from(b)),
            _ => {}
        }
    }
}

fn derive_lines_from_output(
    chunk: &[u8],
    line: &mut String,
    in_escape: &mut bool,
    tx: &std::sync::mpsc::Sender<ActionEvent>,
) {
    for &b in chunk {
        if *in_escape {
            if (b as char).is_ascii_alphabetic() || b == b'~' {
                *in_escape = false;
            }
            continue;
        }

        match b {
            0x1b => {
                *in_escape = true;
            }
            b'\r' | b'\n' => {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !is_prompt_line(trimmed) {
                    let _ = tx.send(ActionEvent::SshOutput {
                        ts_unix_ms: unix_ms(),
                        line: trimmed.to_string(),
                    });
                }
                line.clear();
            }
            0x7f | 0x08 => {
                let _ = line.pop();
            }
            b'\t' => line.push('\t'),
            b if b.is_ascii_graphic() || b == b' ' => line.push(char::from(b)),
            _ => {}
        }
    }
}

fn is_prompt_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed == "$" || trimmed == "#" {
        return true;
    }

    let mut pos = trimmed.rfind('$');
    if pos.is_none() {
        pos = trimmed.rfind('#');
    }

    let Some(pos) = pos else {
        return false;
    };

    if !trimmed[pos + 1..].starts_with(' ') && !trimmed[pos + 1..].is_empty() {
        return false;
    }

    let prefix = &trimmed[..pos];
    prefix.contains('@') && prefix.contains(':')
}

fn write_all_fd(fd: BorrowedFd<'_>, mut data: &[u8]) -> Result<(), Errno> {
    while !data.is_empty() {
        match write(fd, data) {
            Ok(0) => return Err(Errno::EPIPE),
            Ok(n) => data = &data[n..],
            Err(Errno::EINTR) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn wait_for_child_exit(
    child: &mut std::process::Child,
) -> Result<std::process::ExitStatus, std::io::Error> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait();
        }

        std::thread::sleep(Duration::from_millis(25));
    }
}

fn start_actions_event_stream() -> (
    std::sync::mpsc::Sender<ActionEvent>,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<ActionEvent>();
    let writer_thread = std::thread::spawn(move || actions_event_writer(rx));
    (tx, writer_thread)
}

fn send_session_start(tx: &std::sync::mpsc::Sender<ActionEvent>) {
    let _ = tx.send(ActionEvent::SshSessionStart {
        ts_unix_ms: unix_ms(),
        user: std::env::var("USER").unwrap_or_else(|_| "user".into()),
        kind: SshSessionKind::Interactive,
    });
}

fn send_session_end(tx: &std::sync::mpsc::Sender<ActionEvent>, exit_code: i32) {
    let _ = tx.send(ActionEvent::SshSessionEnd {
        ts_unix_ms: unix_ms(),
        exit_code,
    });
}

fn actions_event_writer(rx: std::sync::mpsc::Receiver<ActionEvent>) {
    let mut stream = connect_actions_sink();
    let Some(s) = stream.as_mut() else {
        return;
    };

    for ev in rx {
        send_event(s, &ev);
    }
}

fn connect_actions_sink() -> Option<UnixStream> {
    for _ in 0..20 {
        if let Ok(stream) = UnixStream::connect(ACTIONS_SOCK_PATH) {
            return Some(stream);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn send_event(stream: &mut UnixStream, ev: &ActionEvent) {
    if let Ok(json) = serde_json::to_string(ev) {
        let _ = writeln!(stream, "{json}");
        let _ = stream.flush();
    }
}

fn unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn is_tty(fd: RawFd) -> bool {
    unsafe { nix::libc::isatty(fd) == 1 }
}

fn to_io_err(e: nix::Error) -> std::io::Error {
    std::io::Error::other(e.to_string())
}
