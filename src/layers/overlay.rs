use super::layer_a::LayerA;
use super::layer_c::LayerC;
use super::layer_d::LayerD;
use super::layer_f::LayerF;
use super::layer_s::LayerS;
use super::layer_t::LayerT;
use parking_lot::RwLock;

/// MantleOverlay manages all the modification layers together.
pub struct MantleOverlay {
    pub layer_a: RwLock<LayerA>,
    pub layer_t: RwLock<LayerT>,
    pub layer_c: RwLock<LayerC>,
    pub layer_f: RwLock<LayerF>,
    pub layer_d: RwLock<LayerD>,
    pub layer_s: RwLock<LayerS>,
}

impl MantleOverlay {
    pub fn new() -> Self {
        // Start newly created inodes at 1 Billion to avoid colliding with backend inodes.
        let start_new_ino = 1_000_000_000;
        Self {
            layer_a: RwLock::new(LayerA::new()),
            layer_t: RwLock::new(LayerT::new()),
            layer_c: RwLock::new(LayerC::new()),
            layer_f: RwLock::new(LayerF::new(start_new_ino)),
            layer_d: RwLock::new(LayerD::new()),
            layer_s: RwLock::new(LayerS::new()),
        }
    }
}
