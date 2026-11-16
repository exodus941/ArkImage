//! Message boxes. On Windows they call MessageBoxW directly: rfd's message dialog hung
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
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(h: isize, text: *const u16, cap: *const u16, ty: u32) -> i32;
    }
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let owner = parent
        .and_then(|p| p.window_handle().ok())
        .and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        })
        .unwrap_or(0);
    // MB_ICONINFORMATION 0x40, MB_ICONWARNING 0x30, MB_ICONERROR 0x10; MB_YESNO 0x4;
    // MB_SETFOREGROUND 0x10000
    let icon = match kind {
        Kind::Info => 0x40,
        Kind::Warn => 0x30,
        Kind::Error => 0x10,
    };
    let ty = icon | if yes_no { 0x4 } else { 0 } | 0x10000;
    let (t, c) = (w(text), w(title));
    crate::trace(&format!("message box '{title}', owner {owner:#x}"));
    // IDYES = 6
    let r = unsafe { MessageBoxW(owner, t.as_ptr(), c.as_ptr(), ty) };
    crate::trace(&format!("message box '{title}' returned {r}"));
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
