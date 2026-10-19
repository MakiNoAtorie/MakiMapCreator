//! Draw passes a map kind declares: one shader pass per `RenderPass`.

use mmc_core::MapKindModule;

use crate::{Renderer, ShaderPass};

impl Renderer {
    /// Registers `kind`'s draw passes, bottom first.
    pub fn register_kind(&mut self, kind: &dyn MapKindModule) {
        for p in kind.render_passes() {
            let pass = ShaderPass::new(&self.device, p.shader);
            self.register(p.layer, Box::new(pass));
        }
    }
}
