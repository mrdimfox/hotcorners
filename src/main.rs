//! A simple hot corners implementation for Windows 10/11
#![cfg(windows)]
#![windows_subsystem = "windows"]
#![warn(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]
// #![warn(clippy::all)]
// #![warn(clippy::pedantic)]

mod hotkeys;

use crate::hotkeys::{RMENU_TAB_INPUT, RWIN_TAB_INPUT};

use once_cell::sync::OnceCell;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::{
    core::Result,
    Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Win32::Graphics::Gdi::PtInRect,
    Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, GetKeyboardState, RegisterHotKey, SendInput, HOT_KEY_MODIFIERS,
        INPUT, MOD_ALT, MOD_CONTROL, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_LBUTTON, VK_LWIN,
        VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT,
    },
    Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, SetWindowsHookExW,
        UnhookWindowsHookEx, HHOOK, MSG, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_HOTKEY,
        WM_MOUSEMOVE,
    },
    Win32::System::Console
};

/// How long the cursor must stay within the hot corner to activate, in milliseconds
static mut HOT_DELAY: Duration = Duration::from_millis(50);
/// Base key for exiting
const EXIT_HOTKEY: VIRTUAL_KEY = VK_C;
/// Modifier key(s) for exiting
const EXIT_HOTKEY_MODIFIERS: HOT_KEY_MODIFIERS =
    HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0);

/// Rectangle to define our hot corner
const HOT_CORNER: RECT = RECT {
    left: -500,
    top: -500,
    right: 20,
    bottom: 20,
};

/// Input sequence to send when the hot corner is activated
const HOT_CORNER_INPUT: [INPUT; 4] = RWIN_TAB_INPUT;

#[derive(Debug)]
struct MouseEvent {
    is_mouse_in_corner: bool,
    is_mouse_pressed: bool,
    is_modifier_pressed: bool,
}

// Global handle to the activation thread
static HOT_CORNER_THREAD: OnceCell<JoinHandle<()>> = OnceCell::new();

static HOT_CORNER_THREAD_FLAG: OnceCell<Arc<AtomicBool>> = OnceCell::new();

fn main() -> Result<()> {
    // Make printing to stdout available
    #[cfg(debug_assertions)]
    enable_console();

    // init statics
    HOT_CORNER_THREAD_FLAG
        .set(Arc::new(AtomicBool::new(false)))
        .unwrap();

    HOT_CORNER_THREAD
        .set(thread::spawn(|| {
            let flag = HOT_CORNER_THREAD_FLAG.get().unwrap().clone();
            loop {
                while !flag.load(Ordering::Acquire) {
                    thread::park();
                }
                hot_corner_fn();
                flag.store(false, Ordering::Release);
            }
        }))
        .unwrap();

    unsafe {
        let mut msg: MSG = MSG::default();
        let mouse_hook =
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_callback), HINSTANCE(0), 0)?;

        RegisterHotKey(
            HWND::default(),
            1,
            EXIT_HOTKEY_MODIFIERS,
            EXIT_HOTKEY.0.into(),
        );

        while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
            if msg.message == WM_HOTKEY {
                break;
            }

            DispatchMessageW(&msg);
        }

        UnhookWindowsHookEx(mouse_hook);
    }

    Ok(())
}

/// Runs in a separate thread when the cursor enters the hot corner, and waits to see if it stays there.
/// Will send `HOT_CORNER_INPUT` if the cursor stays within the rectangle defined by `HOT_CORNER` for
/// `HOT_DELAY` milliseconds.
///
/// Note: we've already checked that no modifier keys or mouse buttons are currently pressed in
/// `mouse_hook_callback`.
fn hot_corner_fn() {
    let mut point: POINT = POINT::default();
    let sleep_delay = unsafe { HOT_DELAY };

    thread::sleep(sleep_delay);

    unsafe {
        // Grab cursor position
        if !GetCursorPos(&mut point).as_bool() {
            return;
        }

        // Check if cursor is still in the hot corner and then send the input sequence
        if is_mouse_in_corner(&point) && !send_input(&HOT_CORNER_INPUT) {
            println!("Failed to send input");
        }
    }
}

static mut STILL_HOT: bool = false;

/// Callback that is registered with Windows in order to start the hot corner activation
#[allow(unused_assignments)] // Clippy doesn't like that we sometimes don't read `hot_corner_thread`'s value
extern "system" fn mouse_hook_callback(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    unsafe {
        let evt: *mut MSLLHOOKSTRUCT = std::mem::transmute(l_param);
        let flag = HOT_CORNER_THREAD_FLAG.get().unwrap().clone();

        // If the mouse hasn't moved, we're done
        let wm_evt = u32::try_from(w_param.0).expect("w_param.0 fits in a u32");
        if wm_evt != WM_MOUSEMOVE {
            return CallNextHookEx(HHOOK::default(), n_code, w_param, l_param);
        }

        let event = MouseEvent {
            is_mouse_in_corner: is_mouse_in_corner(&(*evt).pt),
            is_mouse_pressed: is_mouse_pressed(),
            is_modifier_pressed: is_modifier_pressed(),
        };

        // println!("x: {}, y {}", (*evt).pt.x, (*evt).pt.y);

        // Check if the cursor is hot or cold
        if !event.is_mouse_in_corner {
            STILL_HOT = false;
            return CallNextHookEx(HHOOK::default(), n_code, w_param, l_param);
        }

        // The corner is hot, check if it was already hot
        if STILL_HOT {
            return CallNextHookEx(HHOOK::default(), n_code, w_param, l_param);
        }

        // Check if a mouse button is pressed
        if event.is_mouse_pressed {
            return CallNextHookEx(HHOOK::default(), n_code, w_param, l_param);
        }

        // Check if a modifier key is pressed
        if event.is_modifier_pressed {
            return CallNextHookEx(HHOOK::default(), n_code, w_param, l_param);
        }

        println!("{event:?}");

        // The corner is hot, and was previously cold. Notify the worker thread to resume
        flag.store(true, Ordering::Relaxed);
        HOT_CORNER_THREAD.get().unwrap().thread().unpark();

        STILL_HOT = true;
        CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
    }
}

fn is_modifier_pressed() -> bool {
    let mut keystate = [0u8; 256];

    unsafe {
        GetKeyboardState(&mut keystate).as_bool()
            && (keydown(keystate[VK_SHIFT.0 as usize])
                || keydown(keystate[VK_CONTROL.0 as usize])
                || keydown(keystate[VK_MENU.0 as usize])
                || keydown(keystate[VK_LWIN.0 as usize])
                || keydown(keystate[VK_RWIN.0 as usize]))
    }
}

fn is_mouse_pressed() -> bool {
    unsafe {
        (GetKeyState(i32::from(VK_LBUTTON.0)) < 0)
            || (GetKeyState(i32::from(VK_RBUTTON.0)) < 0)
    }
}

fn is_mouse_in_corner(point: &POINT) -> bool {
    unsafe { PtInRect(&HOT_CORNER, *point).as_bool() }
}

fn send_input(input: &[INPUT]) -> bool {
    unsafe {
        // the size of `INPUT` will never exceed an i32
        let size_of_input = i32::try_from(std::mem::size_of::<INPUT>()).unwrap();
        // it would be absurd if the length of HOT_CORNER_INPUT exceeded a u32
        let input_len = u32::try_from(input.len()).unwrap();

        SendInput(input, size_of_input) == input_len
    }
}

/// Convenience function for checking if a key is currently pressed down
#[doc(hidden)]
fn keydown(key: u8) -> bool {
    (key & 0x80) != 0
}

#[cfg(debug_assertions)]
fn enable_console() {
    unsafe {
        Console::FreeConsole();
        Console::AttachConsole(Console::ATTACH_PARENT_PROCESS);
        Console::AllocConsole().unwrap();
    }
}
