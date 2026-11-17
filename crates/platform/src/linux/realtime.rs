use std::{env, fs};

pub fn disable_rtkit() {
    set_while_alone("DISABLE_RTKIT", "1");
}

fn set_while_alone(key: &str, value: &str) {
    if alone() {
        unsafe { env::set_var(key, value) };
    }
}

fn alone() -> bool {
    fs::read_dir("/proc/self/task").is_ok_and(|threads| threads.take(2).count() == 1)
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
