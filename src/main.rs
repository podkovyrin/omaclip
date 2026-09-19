use omaclip::{Cancel, Result, adb::Adb};
use std::{
    fs::OpenOptions,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::unix::pipe,
    sync::{mpsc, watch},
};
#[tokio::main(flavor = "current_thread")]
async fn main() {
    // No payload-bearing core files, child tracing, or Wayland wire logging.
    // SAFETY: startup precedes all spawned tasks/threads.
    unsafe {
        libc::umask(0o077);
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::setrlimit(libc::RLIMIT_CORE, &limit);
        std::env::remove_var("WAYLAND_DEBUG");
        std::env::remove_var("ADB_TRACE");
    }
    let mut args = std::env::args().skip(1);
    let mut adb = String::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--adb" => adb = args.next().unwrap_or_default(),
            "--verify-server" => {
                match omaclip::transport::verify(&omaclip::transport::server_path()).await {
                    Ok(()) => println!("System scrcpy 4.1 server: OK"),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            "--probe-wayland" => {
                match omaclip::wayland::probe().await {
                    Ok(v) => {
                        for line in v {
                            println!("{line}")
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            "--version" => {
                println!("omaclip {} (scrcpy 4.1)", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {
                eprintln!(
                    "Usage: omaclip [--adb PATH] [--version | --verify-server | --probe-wayland]"
                );
                std::process::exit(2);
            }
        }
    }
    if let Err(error) = run(adb).await {
        println!(
            "{}",
            serde_json::json!({"state":"error","message":error,"devices":[],"enabled":false,"selected":"","qrImage":"","qrActive":false})
        );
        std::process::exit(1);
    }
}
async fn run(path: String) -> Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or("XDG_RUNTIME_DIR missing; run inside a Wayland session")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(std::path::Path::new(&runtime).join("omaclip.lock"))
        .map_err(|_| "Cannot open bridge lock")?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Another OmaClip bridge is running; stop it before starting another");
    }
    // Omarchy supplies pipes. Do not use Tokio's uncancellable blocking stdin thread.
    use std::os::fd::BorrowedFd;
    let stdin = unsafe { BorrowedFd::borrow_raw(0) }
        .try_clone_to_owned()
        .map_err(|_| "Cannot open command pipe")?;
    let stdout = unsafe { BorrowedFd::borrow_raw(1) }
        .try_clone_to_owned()
        .map_err(|_| "Cannot open status pipe")?;
    let stdin = pipe::Receiver::from_owned_fd(stdin).map_err(|_| "Run backend with piped stdin")?;
    let mut stdout =
        pipe::Sender::from_owned_fd(stdout).map_err(|_| "Run backend with piped stdout")?;
    let cancel = Cancel::new();
    let sig_cancel = cancel.clone();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "Cannot register shutdown signal")?;
    let signal = tokio::spawn(async move {
        tokio::select! {_=term.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
        sig_cancel.cancel();
    });
    let (tx, rx) = mpsc::channel(8);
    let input = tokio::spawn(async move {
        let mut input = BufReader::new(stdin);
        loop {
            let mut line = Vec::new();
            match (&mut input).take(8193).read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(n) if n > 8192 => {
                    let _ = tx
                        .send(Err("Oversized JSON command; bridge input closed"))
                        .await;
                    break;
                }
                _ => {
                    let value = serde_json::from_slice(&line).map_err(|_| "Invalid JSON command");
                    if tx.send(value).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let (output, mut statuses) = watch::channel(String::new());
    let out_cancel = cancel.clone();
    let writer = tokio::spawn(async move {
        while statuses.changed().await.is_ok() {
            let line = statuses.borrow_and_update().clone() + "\n";
            let result =
                tokio::time::timeout(Duration::from_secs(3), stdout.write_all(line.as_bytes()))
                    .await;
            if !matches!(result, Ok(Ok(()))) {
                out_cancel.cancel();
                break;
            }
        }
    });
    let result = omaclip::daemon::run(
        Adb::new(path)?,
        omaclip::config::Config::path(),
        rx,
        output,
        cancel,
    )
    .await;
    input.abort();
    let _ = input.await;
    signal.abort();
    let _ = signal.await;
    let _ = writer.await;
    drop(lock);
    result
}
