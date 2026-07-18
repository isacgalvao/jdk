//! Disposable registry subkeys for hermetic tests. THE golden rule of the
//! suite: no test may touch the real `HKCU\Environment`, the real PATH or
//! broadcast a real settings change — everything registry goes through a
//! [`TestKey`] under `HKCU\Software`, created here and deleted on drop.

use jdk_core::env::{RegKey, hkcu};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct TestKey {
    pub key: RegKey,
    path: String,
}

impl TestKey {
    pub fn create() -> TestKey {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let path = format!(
            r"Software\jdk-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let (key, _) = hkcu()
            .create_subkey(&path)
            .expect("create disposable test subkey under HKCU\\Software");
        TestKey { key, path }
    }

    /// Subkey path under HKCU — what a hermetic binary run receives via
    /// `JDK_ENV_KEY` / `JDK_MACHINE_ENV_KEY`.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for TestKey {
    fn drop(&mut self) {
        let _ = hkcu().delete_subkey_all(&self.path);
    }
}
