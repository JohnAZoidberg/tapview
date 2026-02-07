use egui::Pos2;

pub struct Dimensions {
    pub touchpad_max_extent_x: f32,
    pub touchpad_max_extent_y: f32,
    pub screen_width: f32,
    pub screen_height: f32,
    pub margin: f32,
}

impl Default for Dimensions {
    fn default() -> Self {
        Self {
            touchpad_max_extent_x: 1345.0,
            touchpad_max_extent_y: 865.0,
            screen_width: 672.0,
            screen_height: 432.0,
            margin: 15.0,
        }
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
