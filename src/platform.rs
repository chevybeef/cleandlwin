//! Native resource ownership and shell operations live here.
#![allow(non_snake_case)]
use anyhow::{Result, bail};
use std::{
    fs::{File, OpenOptions},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{
        Foundation::{E_ABORT, HWND},
        System::Com::*,
        UI::Shell::*,
    },
    core::{HRESULT, PCWSTR, Ref, implement},
};
use windows_sys::Win32::{
    Foundation::LocalFree, Security::Cryptography::*, Storage::FileSystem::*,
};

pub fn wide(text: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}
pub struct Com;
impl Com {
    pub fn init() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
pub fn file_identity(file: &File) -> Result<(u32, u64)> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}
pub fn open_source_locked(path: &Path) -> Result<File> {
    // DELETE plus GENERIC_READ, sharing reads only: no writers or path replacement.
    Ok(OpenOptions::new()
        .access_mode(0x8000_0000 | 0x0001_0000)
        .share_mode(FILE_SHARE_READ)
        .open(path)?)
}
pub fn delete_open_source(file: &File) -> Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            &disposition as *const _ as _,
            std::mem::size_of_val(&disposition) as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub fn replace_file(from: &Path, to: &Path) -> Result<()> {
    if unsafe {
        MoveFileExW(
            wide(from).as_ptr(),
            wide(to).as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub fn protect(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt(bytes, true)
}
pub fn unprotect(bytes: &[u8]) -> Result<Vec<u8>> {
    crypt(bytes, false)
}
fn crypt(bytes: &[u8], protect: bool) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into()?,
        pbData: bytes.as_ptr() as _,
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(result)
}
pub fn choose_folder(owner: isize) -> Result<Option<String>> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)?;
        match dialog.Show(Some(HWND(owner as _))) {
            Ok(()) => {}
            Err(e) if e.code() == HRESULT(0x800704C7u32 as i32) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let item = dialog.GetResult()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let result = name.to_string();
        CoTaskMemFree(Some(name.as_ptr() as _));
        Ok(Some(result?))
    }
}
pub fn open(owner: isize, target: &str) -> Result<()> {
    unsafe {
        let result = ShellExecuteW(
            Some(HWND(owner as _)),
            PCWSTR(wide("open").as_ptr()),
            PCWSTR(wide(target).as_ptr()),
            None,
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            bail!("Windows could not open {target}");
        }
    }
    Ok(())
}

// Reject Shell's permanent-delete fallback in PreDeleteItem. Merely setting
// ALLOWUNDO is insufficient on shares or volumes without a Recycle Bin.
#[implement(IFileOperationProgressSink)]
struct RecycleSink {
    result: Arc<Mutex<Option<HRESULT>>>,
}
impl IFileOperationProgressSink_Impl for RecycleSink_Impl {
    fn PreDeleteItem(&self, flags: u32, _: Ref<IShellItem>) -> windows::core::Result<()> {
        if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
            return Err(windows::core::Error::from_hresult(E_ABORT));
        }
        Ok(())
    }
    fn PostDeleteItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        hr: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        if let Ok(mut result) = self.result.lock() {
            *result = Some(hr);
        }
        hr.ok()
    }
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Err(E_ABORT.into())
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Err(E_ABORT.into())
    }
    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Err(E_ABORT.into())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Err(E_ABORT.into())
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}
pub fn recycle(path: &Path) -> Result<()> {
    let _com = Com::init()?;
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(
            FOFX_RECYCLEONDELETE
                | FOFX_EARLYFAILURE
                | FOF_NOERRORUI
                | FOF_SILENT
                | FOF_NOCONFIRMATION,
        )?;
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide(path).as_ptr()), None)?;
        let result = Arc::new(Mutex::new(None));
        let sink: IFileOperationProgressSink = RecycleSink {
            result: result.clone(),
        }
        .into();
        operation.DeleteItem(&item, &sink)?;
        operation.PerformOperations()?;
        if operation.GetAnyOperationsAborted()?.as_bool() {
            bail!("Recycling unavailable or cancelled; file was retained");
        }
        let status = *result
            .lock()
            .map_err(|_| anyhow::anyhow!("Recycle status unavailable"))?;
        match status {
            Some(hr) if hr.is_ok() => Ok(()),
            Some(hr) => Err(windows::core::Error::from_hresult(hr).into()),
            None => bail!("Windows did not confirm recycling"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recycle_sink_rejects_permanent_delete() {
        let sink: IFileOperationProgressSink = RecycleSink {
            result: Arc::new(Mutex::new(None)),
        }
        .into();
        unsafe {
            assert!(sink.PreDeleteItem(0, None).is_err());
            assert!(
                sink.PreDeleteItem(TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32, None)
                    .is_ok()
            );
        }
    }
    #[test]
    #[ignore = "Recycles one disposable file; run explicitly on a Windows desktop"]
    fn shell_recycles_disposable_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cleandlwin-recycle-smoke.tmp");
        std::fs::write(&path, b"Disposable CleanDL recycle test").unwrap();
        recycle(&path).unwrap();
        assert!(!path.exists());
    }
}
