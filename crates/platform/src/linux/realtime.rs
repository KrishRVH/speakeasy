use std::{env, fs};

pub fn disable_rtkit() {
    if alone() {
        unsafe { env::set_var("DISABLE_RTKIT", "1") };
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
        let before = env::var_os("DISABLE_RTKIT");
        let (release, released) = mpsc::channel::<()>();
        thread::scope(|scope| {
            scope.spawn(move || released.recv());
            disable_rtkit();
            drop(release);
        });
        assert_eq!(env::var_os("DISABLE_RTKIT"), before);
    }
}
