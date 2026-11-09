//! Android KernelSU daemon runtime orchestrating `epoll`, `inotify`, `timerfd`, and `SurfaceFlinger`.

use crate::evdev::EvdevManager;
use crate::power::apply_usb_power_policy_with_config;
use crate::surface::{DualSpriteOverlay, SurfaceError, detect_native_display_bounds};
use crate::sys::{
    CLOCK_MONOTONIC, EPOLL_CLOEXEC, EPOLLIN, EpollEvent, IN_ATTRIB, IN_CLOEXEC, IN_CLOSE_WRITE,
    IN_CREATE, IN_DELETE, IN_MODIFY, IN_MOVED_TO, IN_NONBLOCK, OwnedFd, TFD_CLOEXEC, TFD_NONBLOCK,
    arm_oneshot_timerfd_ms, disarm_timerfd, drain_timerfd, epoll_add, epoll_create1, epoll_wait,
    inotify_add_watch, inotify_init1, read, set_process_name, timerfd_create,
};
use pencursor_core::{Config, OverlayBackend, PlatformRuntime, global_tracker};
use std::ffi::{CString, c_int, c_void};
use std::fmt;
use std::path::Path;
use std::thread;
use std::time::Duration;

const TAG_INOTIFY: u64 = u64::MAX - 1;
const TAG_TIMER: u64 = u64::MAX - 2;

/// Error type for [`AndroidRuntime`].
#[derive(Debug)]
pub struct AndroidRuntimeError(pub String);

impl fmt::Display for AndroidRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AndroidRuntimeError {}

impl From<SurfaceError> for AndroidRuntimeError {
    fn from(err: SurfaceError) -> Self {
        Self(err.0)
    }
}

/// Registers `inotify` watches on the configuration file and its parent directory for live hot-reload.
fn add_config_watches(inotify_fd: c_int, config_path: &Path) {
    let watch_mask = IN_CLOSE_WRITE | IN_MOVED_TO | IN_CREATE | IN_MODIFY | IN_ATTRIB;
    if let Some(parent) = config_path.parent()
        && let Ok(c_parent) = CString::new(parent.to_string_lossy().as_bytes())
    {
        // SAFETY: `c_parent` is a valid NUL-terminated path passed to `inotify_add_watch`.
        unsafe {
            let _ = inotify_add_watch(inotify_fd, c_parent.as_ptr(), watch_mask);
        }
    }
    if let Ok(c_file) = CString::new(config_path.to_string_lossy().as_bytes()) {
        // SAFETY: `c_file` is a valid NUL-terminated path passed to `inotify_add_watch`.
        unsafe {
            let _ = inotify_add_watch(inotify_fd, c_file.as_ptr(), watch_mask);
        }
    }
}

/// Android KernelSU daemon runtime implementation.
#[derive(Debug, Default)]
pub struct AndroidRuntime;

impl PlatformRuntime for AndroidRuntime {
    type Error = AndroidRuntimeError;

    fn run(mut config: Config) -> Result<(), Self::Error> {
        set_process_name("pencursor");

        let config_path = Config::resolve_config_path();
        if config_path.exists() {
            config = Config::load_or_default(&config_path);
        }

        if !config.daemon_enabled {
            println!("[Info] PenCursor KernelSU Daemon 已在配置中禁用 (daemon_enabled=false)");
            return Ok(());
        }

        let tracker = global_tracker();
        tracker.configure_from(&config);

        apply_usb_power_policy_with_config(&config.usb_power);

        let viewport = detect_native_display_bounds();

        // Retry SurfaceFlinger connection up to 15 times during early post-boot initialization.
        let mut overlay_opt = None;
        for attempt in 1..=15 {
            match DualSpriteOverlay::create(&config) {
                Ok(ov) => {
                    overlay_opt = Some(ov);
                    break;
                }
                Err(e) => {
                    if attempt == 15 {
                        return Err(AndroidRuntimeError(format!(
                            "初始化 SurfaceFlinger Overlay 失败: {e}"
                        )));
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
        let mut overlay = overlay_opt.expect("overlay initialized");

        // SAFETY: Creates the master epoll instance with `EPOLL_CLOEXEC`.
        let raw_epfd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        if raw_epfd < 0 {
            return Err(AndroidRuntimeError("调用 epoll_create1 失败".to_string()));
        }
        // SAFETY: `raw_epfd` is a valid newly created epoll descriptor.
        let epoll_fd = unsafe { OwnedFd::from_raw(raw_epfd) };

        // SAFETY: Creates a non-blocking inotify descriptor watching `/dev/input` and `config.conf`.
        let raw_infd = unsafe { inotify_init1(IN_NONBLOCK | IN_CLOEXEC) };
        if raw_infd < 0 {
            return Err(AndroidRuntimeError("调用 inotify_init1 失败".to_string()));
        }
        // SAFETY: `raw_infd` is a valid newly created inotify descriptor.
        let inotify_fd = unsafe { OwnedFd::from_raw(raw_infd) };

        let c_input_dir = CString::new("/dev/input").unwrap();
        // SAFETY: Registers an inotify watch on `/dev/input` for device node creation/deletion.
        unsafe {
            let _ = inotify_add_watch(
                inotify_fd.raw(),
                c_input_dir.as_ptr(),
                IN_CREATE | IN_DELETE | IN_ATTRIB,
            );
        }
        add_config_watches(inotify_fd.raw(), &config_path);

        epoll_add(epoll_fd.raw(), inotify_fd.raw(), EPOLLIN, TAG_INOTIFY)
            .map_err(|e| AndroidRuntimeError(format!("注册 inotify 至 epoll 失败: {e}")))?;

        // SAFETY: Creates a monotonic non-blocking timerfd for stylus hover timeout expiration.
        let raw_tfd = unsafe { timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC) };
        if raw_tfd < 0 {
            return Err(AndroidRuntimeError("调用 timerfd_create 失败".to_string()));
        }
        // SAFETY: `raw_tfd` is a valid newly created timerfd descriptor.
        let timer_fd = unsafe { OwnedFd::from_raw(raw_tfd) };

        epoll_add(epoll_fd.raw(), timer_fd.raw(), EPOLLIN, TAG_TIMER)
            .map_err(|e| AndroidRuntimeError(format!("注册 timerfd 至 epoll 失败: {e}")))?;

        let mut evdev_mgr = EvdevManager::new();
        evdev_mgr.scan_and_register_with_config(epoll_fd.raw(), &config.usb_power);

        println!(
            "[Info] PenCursor KernelSU Daemon 已启动 (Viewport: {}x{}, Overlay: 64x64 Dual-Layer SurfaceControl, Config: {})",
            viewport.width as u32,
            viewport.height as u32,
            config_path.display()
        );

        let mut events = [EpollEvent::default(); 16];
        let mut inotify_scratch = [0u8; 512];
        let mut timer_armed = false;

        loop {
            // SAFETY: Blocks indefinitely (`timeout = -1`) until an evdev, inotify, or timerfd event arrives.
            let n_ready = unsafe {
                epoll_wait(
                    epoll_fd.raw(),
                    events.as_mut_ptr(),
                    events.len() as c_int,
                    -1,
                )
            };

            if n_ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(AndroidRuntimeError(format!("调用 epoll_wait 失败: {err}")));
            }

            let mut need_sync = false;

            for ev in &events[..n_ready as usize] {
                match ev.u64_data {
                    TAG_INOTIFY => {
                        // Drain all queued inotify events and check for config hot-reload + `/dev/input` rescan.
                        loop {
                            // SAFETY: Reads raw inotify events into a stack scratch buffer until EAGAIN.
                            let n = unsafe {
                                read(
                                    inotify_fd.raw(),
                                    inotify_scratch.as_mut_ptr().cast::<c_void>(),
                                    inotify_scratch.len(),
                                )
                            };
                            if n <= 0 {
                                break;
                            }
                        }

                        add_config_watches(inotify_fd.raw(), &config_path);

                        if config_path.exists()
                            && let Ok(new_cfg) = Config::load_from_file(&config_path)
                            && new_cfg != config
                        {
                            if !new_cfg.daemon_enabled {
                                println!(
                                    "[Info] 检测到配置 daemon_enabled=false，正在停止 PenCursor Daemon..."
                                );
                                return Ok(());
                            }

                            let visuals_changed = !new_cfg.visuals_eq(&config)
                                || new_cfg.overlay_enabled != config.overlay_enabled;
                            let power_changed = new_cfg.usb_power != config.usb_power;

                            config = new_cfg;
                            tracker.configure_from(&config);

                            if visuals_changed {
                                let _ = overlay.reload_sprites(&config);
                                need_sync = true;
                            }
                            if power_changed {
                                apply_usb_power_policy_with_config(&config.usb_power);
                            }
                        } else if config_path.exists()
                            && !config.overlay_enabled
                            && let Ok(new_cfg) = Config::load_from_file(&config_path)
                            && new_cfg.overlay_enabled != config.overlay_enabled
                        {
                            config.overlay_enabled = new_cfg.overlay_enabled;
                            let _ = overlay.reload_sprites(&config);
                            need_sync = true;
                        }

                        evdev_mgr.scan_and_register_with_config(epoll_fd.raw(), &config.usb_power);
                    }
                    TAG_TIMER => {
                        let _ = drain_timerfd(timer_fd.raw());
                        timer_armed = false;
                        need_sync = true;
                    }
                    device_tag => {
                        if evdev_mgr.drain_device_events(
                            epoll_fd.raw(),
                            device_tag,
                            ev.events,
                            viewport,
                            tracker,
                        ) {
                            need_sync = true;
                        }
                    }
                }
            }

            if need_sync {
                let snapshot = tracker.snapshot(config.hover_timeout_ms, false);
                OverlayBackend::render_frame(&mut overlay, &snapshot, &config)?;

                if snapshot.in_range && config.overlay_enabled {
                    let _ = arm_oneshot_timerfd_ms(timer_fd.raw(), config.hover_timeout_ms + 5);
                    timer_armed = true;
                } else if timer_armed {
                    disarm_timerfd(timer_fd.raw());
                    timer_armed = false;
                }
            }
        }
    }
}
