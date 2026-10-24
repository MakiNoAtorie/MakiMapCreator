//! Draw passes a map kind declares: one shader pass per `RenderPass`.

use mmc_core::MapKindModule;

use crate::{Renderer, ShaderPass};

impl Renderer {
    /// Registers `kind`'s draw passes, bottom first.
    pub fn register_kind(&mut self, kind: &dyn MapKindModule) {
        for p in kind.render_passes() {
            let pass = Box::new(ShaderPass::new(&self.device, p.shader));
            match p.relief {
                Some(r) => self.register_relief(p.layer, r, pass),
                None => self.register(p.layer, pass),
            }
        }
    }
}
