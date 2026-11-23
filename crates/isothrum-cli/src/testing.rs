//! What the command line's tests share: an output they can read back.

use std::io::Write;
use std::sync::{Arc, Mutex};

/// A shared buffer standing in for an output: clones write to the same bytes.
#[derive(Clone, Default)]
pub(crate) struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    /// What was written since the last take.
    pub(crate) fn take(&self) -> String {
        String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
    }
}
