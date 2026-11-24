//! Windows' own Open, Save and folder pickers.

use std::path::PathBuf;
use windows::{
    core::{Interface, HSTRING, PCWSTR},
    Win32::{
        Foundation::HWND,
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER},
        UI::Shell::{Common::COMDLG_FILTERSPEC, *},
    },
};

/// A file to open; `filter` is (label, pattern), e.g. ("Bookmarks", "*.html;*.htm").
pub fn open(hwnd: isize, title: &str, filter: (&str, &str)) -> Option<PathBuf> {
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        filters(&d, filter)?;
        run(&d.cast().ok()?, hwnd, title)
    }
}

/// Where to save; `name` is the suggested file name.
pub fn save(hwnd: isize, title: &str, name: &str, filter: (&str, &str)) -> Option<PathBuf> {
    unsafe {
        let d: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        filters(&d, filter)?;
        d.SetFileName(&HSTRING::from(name)).ok()?;
        if let Some(ext) = filter.1.split(';').next().and_then(|p| p.strip_prefix("*.")) {
            d.SetDefaultExtension(&HSTRING::from(ext)).ok()?;
        }
        d.SetOptions(d.GetOptions().ok()? | FOS_OVERWRITEPROMPT).ok()?;
        run(&d.cast().ok()?, hwnd, title)
    }
}

pub fn folder(hwnd: isize, title: &str) -> Option<PathBuf> {
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        d.SetOptions(d.GetOptions().ok()? | FOS_PICKFOLDERS).ok()?;
        run(&d.cast().ok()?, hwnd, title)
    }
}

unsafe fn filters(d: &IFileDialog, (label, pattern): (&str, &str)) -> Option<()> {
    let (l, p) = (HSTRING::from(label), HSTRING::from(pattern));
    let spec = [COMDLG_FILTERSPEC { pszName: PCWSTR(l.as_ptr()), pszSpec: PCWSTR(p.as_ptr()) }];
    unsafe { d.SetFileTypes(&spec).ok() }
}

unsafe fn run(d: &IFileDialog, hwnd: isize, title: &str) -> Option<PathBuf> {
    unsafe {
        d.SetTitle(&HSTRING::from(title)).ok()?;
        d.Show(Some(HWND(hwnd as _))).ok()?; // an error here is Cancel
        let name = d.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as _));
        path
    }
}
