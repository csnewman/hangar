//! A display that shows nothing: the guest's desktop is drawn by its own
//! compositor and reached over VNC, and the GPU device needs a display to
//! exist. A scanout the guest sets up anyway is given a frame to draw into,
//! one per scanout, which is never shown.

use krun_display::{
    DisplayBackendBasicFramebuffer, DisplayBackendError, DisplayBackendNew, IntoDisplayBackend,
    MAX_DISPLAYS, Rect, ResourceFormat,
};

struct Unseen {
    frames: Vec<Vec<u8>>,
}

impl DisplayBackendNew<()> for Unseen {
    fn new(_: Option<&()>) -> Self {
        Unseen {
            frames: vec![Vec::new(); MAX_DISPLAYS],
        }
    }
}

impl DisplayBackendBasicFramebuffer for Unseen {
    fn configure_scanout(
        &mut self,
        scanout: u32,
        _display_width: u32,
        _display_height: u32,
        width: u32,
        height: u32,
        _format: ResourceFormat,
    ) -> Result<(), DisplayBackendError> {
        let frame = self
            .frames
            .get_mut(scanout as usize)
            .ok_or(DisplayBackendError::InvalidScanoutId)?;
        *frame = vec![0; (width as usize * height as usize * 4).max(1)];
        Ok(())
    }

    fn disable_scanout(&mut self, _scanout: u32) -> Result<(), DisplayBackendError> {
        Ok(())
    }

    fn alloc_frame(&mut self, scanout: u32) -> Result<(u32, &mut [u8]), DisplayBackendError> {
        match self.frames.get_mut(scanout as usize) {
            Some(f) if !f.is_empty() => Ok((0, f.as_mut_slice())),
            _ => Err(DisplayBackendError::InvalidScanoutId),
        }
    }

    fn present_frame(
        &mut self,
        _scanout: u32,
        _frame: u32,
        _damage: Option<&Rect>,
    ) -> Result<(), DisplayBackendError> {
        Ok(())
    }
}

/// The display backend the GPU device is given.
pub fn backend() -> Result<krun::DisplayBackend, String> {
    let b = Unseen::into_display_backend(None);
    // SAFETY: b is a whole krun_display::DisplayBackend, which new copies.
    unsafe {
        krun::DisplayBackend::new(
            (&b as *const krun_display::DisplayBackend).cast(),
            std::mem::size_of_val(&b),
        )
    }
    .map_err(|e| format!("the GPU's display: {e}"))
}
