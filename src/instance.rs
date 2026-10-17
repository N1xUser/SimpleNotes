//! The lock is independent of the executable location and data directory.
use windows_sys::Win32::{Foundation::*, System::Threading::CreateMutexW};

pub const NAME: &str = "Local\\SimpleNotes.SingleInstance";
pub struct Instance(HANDLE);

impl Instance {
    pub fn acquire(name: &str) -> std::io::Result<Option<Self>> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return Ok(None);
            }
            Ok(Some(Self(handle)))
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_process_child() {
        if let Ok(name) = std::env::var("SIMPLENOTES_INSTANCE_TEST") {
            assert!(Instance::acquire(&name).unwrap().is_none());
        }
    }
    #[test]
    fn excludes_second_process_and_releases_on_exit() {
        let name = format!("Local\\SimpleNotes.TestInstance.{}", std::process::id());
        let owner = Instance::acquire(&name).unwrap().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "instance::tests::second_process_child"])
            .env("SIMPLENOTES_INSTANCE_TEST", &name)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        drop(owner);
        assert!(Instance::acquire(&name).unwrap().is_some());
    }
}
