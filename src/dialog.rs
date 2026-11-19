//! Message boxes. On Windows they call TaskDialogIndirect directly: rfd's message dialog hung
//! without showing (first-run install question, crash dialog in the backup).

use winit::window::Window;

#[derive(Clone, Copy)]
pub enum Kind {
    Info,
    Warn,
    Error,
}
#[cfg(windows)]
fn show(parent: Option<&Window>, title: &str, text: &str, kind: Kind, yes_no: bool) -> bool {
    use std::ffi::c_void;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // TASKDIALOGCONFIG, packed to 1 byte as in commctrl.h
    #[repr(C, packed(1))]
    struct Config {
        size: u32,
        owner: isize,
        inst: isize,
        flags: u32,
        buttons: u32,
        title: *const u16,
        main_icon: isize,
        instruction: *const u16,
        content: *const u16,
        n_buttons: u32,
        p_buttons: *const c_void,
        default_button: i32,
        n_radios: u32,
        p_radios: *const c_void,
        default_radio: i32,
        verify: *const u16,
        expanded: *const u16,
        expand_label: *const u16,
        collapse_label: *const u16,
        footer_icon: isize,
        footer: *const u16,
        callback: *const c_void,
        callback_data: isize,
        width: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(h: isize, text: *const u16, cap: *const u16, ty: u32) -> i32;
        fn LoadIconW(inst: isize, name: *const u16) -> isize;
    }
    #[link(name = "comctl32")]
    extern "system" {
        fn TaskDialogIndirect(c: *const Config, button: *mut i32, radio: *mut i32, verify: *mut i32) -> i32;
    }
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let owner = parent
        .and_then(|p| p.window_handle().ok())
        .and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        })
        .unwrap_or(0);
    let (t, c) = (w(text), w(title));
    crate::trace(&format!("message box '{title}', owner {owner:#x}"));
    // A task dialog plays a sound only for its own icon ids (TD_WARNING_ICON and so on).
    // The same system icon passed as a handle (IDI_INFORMATION 32516, IDI_WARNING 32515,
    // IDI_ERROR 32513) shows the same picture and plays nothing.
    let idi: usize = match kind {
        Kind::Info => 32516,
        Kind::Warn => 32515,
        Kind::Error => 32513,
    };
    let icon = unsafe { LoadIconW(0, idi as *const u16) };
    let cfg = Config {
        size: std::mem::size_of::<Config>() as u32,
        owner,
        inst: 0,
        // TDF_USE_HICON_MAIN 0x2, TDF_POSITION_RELATIVE_TO_WINDOW 0x1000
        flags: 0x2 | if owner != 0 { 0x1000 } else { 0 },
        // TDCBF_OK 0x1, TDCBF_YES 0x2, TDCBF_NO 0x4
        buttons: if yes_no { 0x2 | 0x4 } else { 0x1 },
        title: c.as_ptr(),
        main_icon: icon,
        instruction: std::ptr::null(),
        content: t.as_ptr(),
        n_buttons: 0,
        p_buttons: std::ptr::null(),
        default_button: 0,
        n_radios: 0,
        p_radios: std::ptr::null(),
        default_radio: 0,
        verify: std::ptr::null(),
        expanded: std::ptr::null(),
        expand_label: std::ptr::null(),
        collapse_label: std::ptr::null(),
        footer_icon: 0,
        footer: std::ptr::null(),
        callback: std::ptr::null(),
        callback_data: 0,
        width: 0,
    };
    let mut r = 0i32;
    let hr = unsafe { TaskDialogIndirect(&cfg, &mut r, std::ptr::null_mut(), std::ptr::null_mut()) };
    if hr < 0 {
        // no task dialog (an old comctl32): the message box, which plays the sound
        crate::trace(&format!("task dialog failed {hr:#x}; message box"));
        let icon = match kind {
            Kind::Info => 0x40,
            Kind::Warn => 0x30,
            Kind::Error => 0x10,
        };
        let ty = icon | if yes_no { 0x4 } else { 0 } | 0x10000;
        r = unsafe { MessageBoxW(owner, t.as_ptr(), c.as_ptr(), ty) };
    }
    crate::trace(&format!("message box '{title}' returned {r}"));
    // IDYES = 6
    r == 6
}

#[cfg(not(windows))]
fn show(_parent: Option<&Window>, title: &str, text: &str, kind: Kind, yes_no: bool) -> bool {
    let level = match kind {
        Kind::Info => rfd::MessageLevel::Info,
        Kind::Warn => rfd::MessageLevel::Warning,
        Kind::Error => rfd::MessageLevel::Error,
    };
    let d = rfd::MessageDialog::new().set_level(level).set_title(title).set_description(text);
    if yes_no {
        d.set_buttons(rfd::MessageButtons::YesNo).show() == rfd::MessageDialogResult::Yes
    } else {
        d.show();
        false
    }
}

/// Yes/No question; true for Yes.
pub fn ask(parent: Option<&Window>, title: &str, text: &str, kind: Kind) -> bool {
    show(parent, title, text, kind, true)
}

/// A message with an OK button.
pub fn tell(parent: Option<&Window>, title: &str, text: &str, kind: Kind) {
    show(parent, title, text, kind, false);
}
