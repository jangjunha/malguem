//! "Is the user playing a game right now?" for the relayed screen broadcast.
//!
//! A participant in a fullscreen game shouldn't spend CPU and upload
//! relaying someone else's stream: the upload in particular fills the home
//! router's buffer and raises the game's ping. The frontend polls this and
//! stops offering itself as a relay while it returns true (see
//! `client/src/lib/relay/load.ts`).
//!
//! Windows exposes exactly this signal through the shell's notification
//! state, which it uses itself to hold back toasts during games: exclusive
//! Direct3D fullscreen, any fullscreen ("rude") window such as a borderless
//! game, or presentation mode. Declared by hand to avoid a new crate for one
//! function in shell32.

#[tauri::command]
pub fn fullscreen_app_active() -> bool {
    imp::fullscreen_app_active()
}

#[cfg(windows)]
mod imp {
    // QUERY_USER_NOTIFICATION_STATE values (shellapi.h).
    const QUNS_BUSY: i32 = 2; // a fullscreen app is running
    const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;
    const QUNS_PRESENTATION_MODE: i32 = 4;

    #[link(name = "shell32")]
    extern "system" {
        fn SHQueryUserNotificationState(pquns: *mut i32) -> i32; // HRESULT
    }

    pub fn fullscreen_app_active() -> bool {
        let mut state: i32 = 0;
        // SAFETY: writes one i32 through a valid pointer; no other effects.
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        hr >= 0
            && matches!(
                state,
                QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
            )
    }
}

#[cfg(not(windows))]
mod imp {
    /// No equivalent signal wired up; CPU-pressure heuristics cover the rest.
    pub fn fullscreen_app_active() -> bool {
        false
    }
}
