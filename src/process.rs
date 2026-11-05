//! Long-running background jobs whose output is streamed to the interface.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};

/// What a job tells whoever is watching it.
#[derive(Debug, Clone)]
pub enum Event {
    /// A new step began; `fraction` is overall progress if known.
    Stage {
        title: String,
        fraction: Option<f64>,
    },
    /// Progress within the current step, 0.0 to 1.0.
    Progress(f64),
    /// A line of output.
    Line(String),
    /// The job ended.
    Finished(Result<String, String>),
}

/// The sending half a job reports through, and its cancellation flag.
#[derive(Clone)]
pub struct Reporter {
    sender: async_channel::Sender<Event>,
    cancelled: Arc<AtomicBool>,
}

/// Lets the interface stop a job.
#[derive(Clone)]
pub struct Canceller(Arc<AtomicBool>);

impl Canceller {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Reporter {
    pub fn stage(&self, title: impl Into<String>, fraction: Option<f64>) {
        let title = title.into();
        self.line(format!("==> {title}"));
        let _ = self.sender.send_blocking(Event::Stage { title, fraction });
    }

    pub fn progress(&self, fraction: f64) {
        let _ = self
            .sender
            .send_blocking(Event::Progress(fraction.clamp(0.0, 1.0)));
    }

    pub fn line(&self, line: impl Into<String>) {
        let _ = self.sender.send_blocking(Event::Line(line.into()));
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn check_cancelled(&self) -> anyhow::Result<()> {
        if self.is_cancelled() {
            bail!("cancelled");
        }
        Ok(())
    }

    /// Run `command` to completion, streaming its stdout and stderr, and
    /// killing its whole process group if the job is cancelled.
    pub fn run(&self, command: &mut Command) -> anyhow::Result<ExitStatus> {
        self.run_observed(command, |_| {})
    }

    /// Like [`Reporter::run`], also passing each output line to `observe`.
    pub fn run_observed(
        &self,
        command: &mut Command,
        observe: impl Fn(&str) + Send + Sync + 'static,
    ) -> anyhow::Result<ExitStatus> {
        self.run_tracked(command, observe, |_| {})
    }

    /// Like [`Reporter::run_observed`], also passing the pid of the started
    /// command, which leads a process group of its own, to `started`.
    pub fn run_tracked(
        &self,
        command: &mut Command,
        observe: impl Fn(&str) + Send + Sync + 'static,
        started: impl FnOnce(u32),
    ) -> anyhow::Result<ExitStatus> {
        let observe: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(observe);
        self.line(format!("$ {}", describe(command)));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A group of its own, so cancelling reaches cargo's rustc children.
            .process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("could not start {:?}", command.get_program()))?;
        started(child.id());
        let readers = [
            child
                .stdout
                .take()
                .map(|s| self.forward(s, observe.clone())),
            child
                .stderr
                .take()
                .map(|s| self.forward(s, observe.clone())),
        ];
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if self.is_cancelled() {
                // SAFETY: plain syscall on a process group we created.
                unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGTERM) };
                let _ = child.wait();
                bail!("cancelled");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        for reader in readers.into_iter().flatten() {
            let _ = reader.join();
        }
        Ok(status)
    }

    /// Like [`Reporter::run`], but a non-zero exit is an error.
    pub fn run_checked(&self, command: &mut Command) -> anyhow::Result<()> {
        let status = self.run(command)?;
        if !status.success() {
            bail!("{:?} failed ({status})", command.get_program());
        }
        Ok(())
    }

    fn forward(
        &self,
        stream: impl Read + Send + 'static,
        observe: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> std::thread::JoinHandle<()> {
        let reporter = self.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            while reader
                .read_until(b'\n', &mut buf)
                .map(|n| n > 0)
                .unwrap_or(false)
            {
                let text = String::from_utf8_lossy(&buf);
                // Progress bars redraw with carriage returns; keep the last.
                let text = text.trim_end().rsplit('\r').next().unwrap_or_default();
                observe(text);
                reporter.line(text.to_string());
                buf.clear();
            }
        })
    }
}

/// A command line, for the log.
pub fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|a| {
            let a = a.to_string_lossy();
            if a.contains(' ') {
                format!("{a:?}")
            } else {
                a.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run `job` on a worker thread. Its events arrive on the returned receiver,
/// ending with exactly one [`Event::Finished`].
pub fn spawn<F>(job: F) -> (async_channel::Receiver<Event>, Canceller)
where
    F: FnOnce(&Reporter) -> anyhow::Result<String> + Send + 'static,
{
    let (sender, receiver) = async_channel::unbounded();
    let cancelled = Arc::new(AtomicBool::new(false));
    let reporter = Reporter {
        sender,
        cancelled: cancelled.clone(),
    };
    std::thread::spawn(move || {
        let result = job(&reporter).map_err(|e| format!("{e:#}"));
        if let Err(e) = &result {
            reporter.line(format!("error: {e}"));
        }
        let _ = reporter.sender.send_blocking(Event::Finished(result));
    });
    (receiver, Canceller(cancelled))
}

/// Run blocking `work` off the main thread and await its result.
pub async fn background<T, F>(work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    gtk::gio::spawn_blocking(work)
        .await
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(receiver: async_channel::Receiver<Event>) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = receiver.recv_blocking() {
            events.push(event);
        }
        events
    }

    #[test]
    fn streams_output_and_finishes() {
        let (rx, _) = spawn(|r| {
            r.run_checked(Command::new("sh").args(["-c", "echo out; echo err >&2"]))?;
            Ok("done".into())
        });
        let events = collect(rx);
        let lines: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Line(l) => Some(l.as_str()),
                _ => None,
            })
            .collect();
        assert!(lines.contains(&"out"));
        assert!(lines.contains(&"err"));
        assert!(matches!(events.last(), Some(Event::Finished(Ok(s))) if s == "done"));
    }

    #[test]
    fn cancels_a_running_command() {
        let (rx, canceller) = spawn(|r| {
            r.run_checked(Command::new("sh").args(["-c", "sleep 30"]))?;
            Ok(String::new())
        });
        std::thread::sleep(Duration::from_millis(300));
        let started = std::time::Instant::now();
        canceller.cancel();
        let events = collect(rx);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(matches!(events.last(), Some(Event::Finished(Err(e))) if e.contains("cancelled")));
    }
}
