use anyhow::{Context, Result, ensure};
use clap::Parser;
use railwatch::{
    ipc::{Service, serve_client},
    runtime::{self, STOP},
};
use std::{
    fs::{self, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Parser)]
#[command(version, about = "Railwatch telemetry and history daemon")]
struct Args {
    #[arg(long, default_value = "/run/railwatch")]
    runtime_dir: PathBuf,
    #[arg(long, default_value = "/var/lib/railwatch/history.db")]
    database: PathBuf,
    #[arg(long)]
    device: Option<PathBuf>,
    /// Optional Unix group allowed to change local policy and prices.
    #[arg(long)]
    control_group: Option<String>,
    #[arg(long)]
    simulate: bool,
    #[arg(long, requires = "simulate")]
    fault_after: Option<u64>,
    #[arg(long, requires = "simulate")]
    disconnect_after: Option<u64>,
    /// Exit after N seconds, for qualification runs.
    #[arg(long)]
    run_for: Option<u64>,
}
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
fn bind(path: &Path, mode: u32) -> Result<UnixListener> {
    if let Ok(m) = fs::symlink_metadata(path) {
        ensure!(
            m.file_type().is_socket(),
            "refusing to remove non-socket {}",
            path.display()
        );
        ensure!(
            UnixStream::connect(path).is_err(),
            "another daemon is listening at {}",
            path.display()
        );
        fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    l.set_nonblocking(true)?;
    Ok(l)
}
fn main() -> Result<()> {
    let a = Args::parse();
    unsafe {
        libc::umask(0o077);
        libc::signal(libc::SIGTERM, stop as *const () as usize);
        libc::signal(libc::SIGINT, stop as *const () as usize);
    }
    fs::create_dir_all(&a.runtime_dir)?;
    let meta = fs::symlink_metadata(&a.runtime_dir)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "runtime directory must be a directory, not a symlink"
    );
    ensure!(
        meta.permissions().mode() & 0o022 == 0,
        "runtime directory must not be writable by group or others"
    );
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(a.runtime_dir.join("daemon.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another daemon owns this runtime directory"
    );
    if let Some(parent) = a.database.parent() {
        fs::create_dir_all(parent)?;
    }
    let read_path = a.runtime_dir.join("monitor.sock");
    let control_path = a.runtime_dir.join("control.sock");
    let read = bind(&read_path, 0o660)?;
    let control = bind(&control_path, 0o600)?;
    if let Some(group) = &a.control_group {
        let name = std::ffi::CString::new(group.as_str())?;
        let entry = unsafe { libc::getgrnam(name.as_ptr()) };
        ensure!(!entry.is_null(), "control group does not exist: {group}");
        let gid = unsafe { (*entry).gr_gid };
        let path = std::ffi::CString::new(control_path.as_os_str().as_encoded_bytes())?;
        ensure!(
            unsafe { libc::chown(path.as_ptr(), !0, gid) } == 0,
            "set control socket group: {}",
            std::io::Error::last_os_error()
        );
        fs::set_permissions(&control_path, fs::Permissions::from_mode(0o660))?;
    }
    let rt = runtime::start(
        a.database.clone(),
        a.device,
        a.simulate,
        a.fault_after,
        a.disconnect_after,
    )?;
    let service = Service {
        shared: rt.shared.clone(),
        database: a.database,
        commands: rt.commands,
    };
    let clients = Arc::new(AtomicUsize::new(0));
    let started = std::time::Instant::now();
    eprintln!(
        "railwatchd listening on {}{}",
        read_path.display(),
        if a.simulate { " (SIMULATED)" } else { "" }
    );
    while !STOP.load(Ordering::Relaxed) {
        if a.run_for.is_some_and(|n| started.elapsed().as_secs() >= n) {
            STOP.store(true, Ordering::Relaxed);
            break;
        }
        for (listener, admin) in [(&read, false), (&control, true)] {
            match listener.accept() {
                Ok((stream, _)) => {
                    if clients.load(Ordering::Relaxed) >= 32 {
                        drop(stream);
                        continue;
                    }
                    if admin && a.control_group.is_none() {
                        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
                        let mut n = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
                        let ok = unsafe {
                            libc::getsockopt(
                                stream.as_raw_fd(),
                                libc::SOL_SOCKET,
                                libc::SO_PEERCRED,
                                &mut cred as *mut _ as *mut _,
                                &mut n,
                            )
                        };
                        if ok != 0 || !(cred.uid == 0 || cred.uid == unsafe { libc::geteuid() }) {
                            continue;
                        }
                    }
                    clients.fetch_add(1, Ordering::Relaxed);
                    let count = clients.clone();
                    let service = service.clone();
                    std::thread::spawn(move || {
                        let _ = serve_client(stream, &service, admin);
                        count.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).context("accept client"),
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for worker in rt.workers {
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("worker panicked"))?;
    }
    fs::remove_file(read_path)?;
    fs::remove_file(control_path)?;
    Ok(())
}
