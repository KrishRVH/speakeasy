use std::{env, fs};

/// Opts out of `PipeWire`'s realtime module; call it before any other thread starts.
///
/// Loaded through ALSA without realtime privileges, the module lowers `RLIMIT_RTTIME` to the
/// budget the desktop reports, and a zero budget gets the process killed once capture starts.
/// Capture is buffered and needs no realtime scheduling. Child processes inherit the variable.
pub fn disable_rtkit() {
    set_while_alone("DISABLE_RTKIT", "1");
}

fn set_while_alone(key: &str, value: &str) {
    if let Ok(mut tasks) = fs::read_dir("/proc/self/task")
        && matches!(tasks.next(), Some(Ok(_)))
        && tasks.next().is_none()
    {
        // SAFETY: `/proc/self/task` lists one readable entry, this thread, so no other thread can
        // read the environment concurrently. An unreadable entry must not count as this thread.
        unsafe { env::set_var(key, value) };
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread};

    use super::*;

    #[test]
    fn another_running_thread_leaves_the_environment_untouched() {
        let key = "SPEAKEASY_TEST_UNWRITTEN_WHILE_THREADS_RUN";
        let (release, released) = mpsc::channel::<()>();
        thread::scope(|scope| {
            scope.spawn(move || released.recv());
            set_while_alone(key, "1");
            drop(release);
        });
        assert_eq!(env::var_os(key), None);
    }
}
