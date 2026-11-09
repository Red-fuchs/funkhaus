//! Playback via `afplay` child (no audio crate). Only ever kills its own child.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[derive(Default)]
pub struct Player {
    cur: Option<(Child, PathBuf)>,
}

impl Player {
    pub fn play(&mut self, p: &Path) -> std::io::Result<()> {
        self.stop();
        let c = Command::new("afplay").arg(p).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        self.cur = Some((c, p.into()));
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some((mut c, _)) = self.cur.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// File currently playing (reaps a finished child).
    pub fn playing(&mut self) -> Option<&Path> {
        if self.cur.as_mut().is_some_and(|(c, _)| !matches!(c.try_wait(), Ok(None))) {
            self.cur = None;
        }
        self.cur.as_ref().map(|(_, p)| p.as_path())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}
