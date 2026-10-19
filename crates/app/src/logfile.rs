//! Session log in `<app dir>/logs/mmc.log`; the previous session moves to `mmc.prev.log`.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

struct FileLog {
    file: Mutex<File>,
    start: Instant,
}

impl Log for FileLog {
    fn enabled(&self, m: &Metadata) -> bool {
        // Dependencies (wgpu, eframe) log a lot at info; keep only their warnings.
        m.level() <= Level::Warn || m.target().starts_with("mmc")
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let t = self.start.elapsed().as_secs_f64();
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{t:9.3} {:5} {}: {}", r.level(), r.target(), r.args());
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// Starts the log and logs panics; returns the log path, or `None` if the file can not be made.
pub fn init(app_dir: &Path) -> Option<PathBuf> {
    let dir = app_dir.join("logs");
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("mmc.log");
    let _ = fs::rename(&path, dir.join("mmc.prev.log"));
    let file = File::create(&path).ok()?;
    let logger = FileLog {
        file: Mutex::new(file),
        start: Instant::now(),
    };
    log::set_boxed_logger(Box::new(logger)).ok()?;
    log::set_max_level(LevelFilter::Info);
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        log::logger().flush();
        default(info);
    }));
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    log::info!(
        "MakiMapCreator {} started, unix time {unix}",
        env!("CARGO_PKG_VERSION")
    );
    Some(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_lines_and_keeps_the_previous_session() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("mmc.log"), "old session\n").unwrap();
        let path = super::init(dir.path()).unwrap();
        log::info!(target: "mmc_app", "hello");
        log::info!(target: "wgpu_core", "noise");
        log::logger().flush();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("started") && text.contains("hello"));
        assert!(!text.contains("noise"));
        let prev = std::fs::read_to_string(logs.join("mmc.prev.log")).unwrap();
        assert_eq!(prev, "old session\n");
    }
}
