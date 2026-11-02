//! The Mesa "surfaceless" backend, which only supports off-screen surfaces and cannot directly
//! display surfaces on a screen.

pub mod connection;
pub mod context;
pub mod device;
pub mod surface;

crate::implement_interfaces!();

#[cfg(test)]
#[path = "../tests.rs"]
mod tests;

static USE_GLES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Make the surfaceless platform create OpenGL ES 3.x contexts instead of desktop GL.
pub fn set_use_gles(v: bool) {
    USE_GLES.store(v, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn use_gles() -> bool {
    USE_GLES.load(std::sync::atomic::Ordering::SeqCst)
}
