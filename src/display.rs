//! Prompt placement follows one physical output, while coverage spans the root.
use x11rb::{
    protocol::{randr::ConnectionExt as _, xproto::ConnectionExt as _},
    rust_connection::RustConnection,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

impl Area {
    pub fn centre(self) -> (i16, i16) {
        (
            (i32::from(self.x) + i32::from(self.width) / 2).clamp(i16::MIN as i32, i16::MAX as i32)
                as i16,
            (i32::from(self.y) + i32::from(self.height) / 2).clamp(i16::MIN as i32, i16::MAX as i32)
                as i16,
        )
    }

    fn contains(self, point: (i16, i16)) -> bool {
        let (x, y) = (i32::from(point.0), i32::from(point.1));
        x >= i32::from(self.x)
            && y >= i32::from(self.y)
            && x < i32::from(self.x) + i32::from(self.width)
            && y < i32::from(self.y) + i32::from(self.height)
    }
}

fn choose(
    outputs: &[(u32, Area)],
    preferred: Option<u32>,
    point: (i16, i16),
    fallback: Area,
) -> (Option<u32>, Area) {
    outputs
        .iter()
        .find(|(id, _)| Some(*id) == preferred)
        .or_else(|| outputs.iter().find(|(_, area)| area.contains(point)))
        .or_else(|| outputs.first())
        .map(|(id, area)| (Some(*id), *area))
        .unwrap_or((None, fallback))
}

pub fn prompt_area(
    c: &RustConnection,
    root: u32,
    preferred: Option<u32>,
    fallback: Area,
) -> (Option<u32>, Area) {
    let point = c
        .query_pointer(root)
        .ok()
        .and_then(|r| r.reply().ok())
        .map(|r| (r.root_x, r.root_y))
        .unwrap_or_else(|| fallback.centre());
    let mut outputs = Vec::new();
    if let Some(resources) = c
        .randr_get_screen_resources_current(root)
        .ok()
        .and_then(|r| r.reply().ok())
    {
        for output in resources.outputs {
            let Some(info) = c
                .randr_get_output_info(output, resources.config_timestamp)
                .ok()
                .and_then(|r| r.reply().ok())
            else {
                continue;
            };
            if info.crtc == 0 {
                continue;
            }
            if let Some(crtc) = c
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)
                .ok()
                .and_then(|r| r.reply().ok())
            {
                if crtc.width > 0 && crtc.height > 0 {
                    outputs.push((
                        output,
                        Area {
                            x: crtc.x,
                            y: crtc.y,
                            width: crtc.width,
                            height: crtc.height,
                        },
                    ));
                }
            }
        }
    }
    choose(&outputs, preferred, point, fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_stays_on_its_output_and_recovers_after_removal() {
        let left = Area {
            x: -1200,
            y: 0,
            width: 1200,
            height: 900,
        };
        let right = Area {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let outputs = [(1, left), (2, right)];
        assert_eq!(choose(&outputs, None, (-500, 100), right), (Some(1), left));
        assert_eq!(
            choose(&outputs, Some(1), (500, 100), right),
            (Some(1), left)
        );
        assert_eq!(
            choose(&outputs[1..], Some(1), (500, 100), right),
            (Some(2), right)
        );
        assert_eq!(choose(&[], Some(1), (500, 100), right), (None, right));
        assert_eq!(left.centre(), (-600, 450));
    }
}
