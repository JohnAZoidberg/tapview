use crate::config::PtpConfig;
use egui::Pos2;

pub struct Dimensions {
    pub touchpad_max_extent_x: f32,
    pub touchpad_max_extent_y: f32,
    pub screen_width: f32,
    pub screen_height: f32,
    pub margin: f32,
    /// True when extents came from the descriptor's logical range.
    pub extent_known: bool,
}

impl Default for Dimensions {
    fn default() -> Self {
        Self {
            touchpad_max_extent_x: 1345.0,
            touchpad_max_extent_y: 865.0,
            screen_width: 672.0,
            screen_height: 432.0,
            margin: 15.0,
            extent_known: false,
        }
    }
}

impl Dimensions {
    pub fn from_config(config: &Option<PtpConfig>) -> Self {
        let mut dims = Self::default();
        if let Some(cfg) = config {
            if let Some(phys) = &cfg.physical_size {
                dims.touchpad_max_extent_x = phys.x.logical_max as f32;
                dims.touchpad_max_extent_y = phys.y.logical_max as f32;
                dims.extent_known = true;
            }
        }
        dims
    }
}

impl Dimensions {
    pub fn get_touchpad_scale(&self) -> f32 {
        let ratio_screen = self.screen_width / self.screen_height;
        let ratio_touchpad = self.touchpad_max_extent_x / self.touchpad_max_extent_y;

        if ratio_screen > ratio_touchpad {
            self.screen_height / (self.touchpad_max_extent_y + self.margin * 2.0)
        } else {
            self.screen_width / (self.touchpad_max_extent_x + self.margin * 2.0)
        }
    }

    pub fn get_touchpad_corner(&self, scale: f32) -> Pos2 {
        Pos2::new(
            self.screen_width / 2.0 - (self.touchpad_max_extent_x / 2.0) * scale,
            self.screen_height / 2.0 - (self.touchpad_max_extent_y / 2.0) * scale,
        )
    }

    pub fn maybe_grow_touchpad_extent(&mut self, x: f32, y: f32) {
        if self.touchpad_max_extent_x < x {
            self.touchpad_max_extent_x = x;
        }
        if self.touchpad_max_extent_y < y {
            self.touchpad_max_extent_y = y;
        }
    }
}
