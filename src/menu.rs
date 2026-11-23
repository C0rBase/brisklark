//! Native right-click menus, dark when Windows is.

use windows::{
    core::{w, HSTRING, PCSTR, PCWSTR},
    Win32::{
        Foundation::{HWND, POINT},
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
        UI::WindowsAndMessaging::*,
    },
};

pub enum M {
    Item(u32, String, bool),        // id, label, enabled
    Check(u32, String, bool, bool), // id, label, checked, enabled
    Sep,
    Sub(String, Vec<M>), // greyed out when empty
}

fn label(s: &str) -> HSTRING {
    let s: String = if s.chars().count() > 60 { s.chars().take(57).chain("…".chars()).collect() } else { s.into() };
    HSTRING::from(s.replace('&', "&&")) // a lone & would underline the next letter
}

unsafe fn build(items: &[M]) -> windows::core::Result<HMENU> {
    let off = |on: bool| if on { MF_STRING } else { MF_GRAYED };
    unsafe {
        let m = CreatePopupMenu()?;
        for it in items {
            match it {
                M::Item(id, text, on) => AppendMenuW(m, off(*on), *id as usize, &label(text))?,
                M::Check(id, text, checked, on) => {
                    let check = if *checked { MF_CHECKED } else { MF_UNCHECKED };
                    AppendMenuW(m, off(*on) | check, *id as usize, &label(text))?
                }
                M::Sep => AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null())?,
                M::Sub(text, kids) => {
                    let sub = build(kids)?;
                    AppendMenuW(m, MF_POPUP | off(!kids.is_empty()), sub.0 as usize, &label(text))?
                }
            }
        }
        Ok(m)
    }
}

/// Shows the menu at the pointer and returns the picked id (0: none).
pub fn popup(hwnd: isize, dark: bool, items: &[M]) -> u32 {
    unsafe {
        theme(dark);
        let Ok(m) = build(items) else { return 0 };
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let picked = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, None, HWND(hwnd as _), None);
        let _ = DestroyMenu(m); // and its submenus
        picked.0 as u32
    }
}

/// Menus follow the app's light or dark mode through uxtheme's unnamed exports
/// (#135 SetPreferredAppMode, #136 FlushMenuThemes), which Explorer's own use.
/// On a Windows without them, menus are simply light.
unsafe fn theme(dark: bool) {
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let (Some(set), Some(flush)) = (GetProcAddress(ux, PCSTR(135 as _)), GetProcAddress(ux, PCSTR(136 as _))) {
            let set: extern "system" fn(i32) -> i32 = std::mem::transmute(set);
            let flush: extern "system" fn() = std::mem::transmute(flush);
            set(if dark { 2 } else { 3 }); // ForceDark, ForceLight
            flush();
        }
    }
}
