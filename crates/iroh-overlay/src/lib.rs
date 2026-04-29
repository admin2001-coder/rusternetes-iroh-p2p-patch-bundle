use anyhow::Result;

pub struct Overlay;

impl Overlay {
    pub async fn new() -> Self {
        Overlay
    }

    pub async fn run(&self) -> Result<()> {
        // Placeholder for overlay networking runtime.
        Ok(())
    }
}
