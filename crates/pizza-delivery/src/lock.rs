use std::fmt;
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Poisoned {
    name: &'static str,
}

impl fmt::Display for Poisoned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the {} lock is poisoned: another thread panicked while holding it",
            self.name
        )
    }
}

impl std::error::Error for Poisoned {}

impl From<Poisoned> for String {
    fn from(poisoned: Poisoned) -> Self {
        poisoned.to_string()
    }
}

pub fn acquire<'a, T>(
    mutex: &'a Mutex<T>,
    name: &'static str,
) -> Result<MutexGuard<'a, T>, Poisoned> {
    mutex.lock().map_err(|_| Poisoned { name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_healthy_lock_is_acquired() {
        let m = Mutex::new(1);
        assert_eq!(acquire(&m, "counter").map(|g| *g), Ok(1));
    }

    #[test]
    fn a_poisoned_lock_names_itself_in_the_error() {
        let m = Arc::new(Mutex::new(1));
        let held = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _guard = held.lock();
            std::panic::resume_unwind(Box::new("boom"));
        })
        .join();
        let message = acquire(&m, "store").map(|_| ()).map_err(|e| e.to_string());
        assert_eq!(
            message,
            Err("the store lock is poisoned: another thread panicked while holding it".to_owned())
        );
    }
}
