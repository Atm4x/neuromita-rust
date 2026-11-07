//! Interactive Guide focus geometry imported from the original Python GuideWidget.
//! Coordinates are normalized to each page's cropped screenshot.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusRect {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageCrop {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl ImageCrop {
    pub const FULL: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusStep {
    pub number: u8,
    pub rect: FocusRect,
}

pub fn crop(page_id: &str) -> ImageCrop {
    match page_id {
        "memoryrag" => ImageCrop {
            left: 0.0200,
            top: 0.0800,
            right: 0.9800,
            bottom: 0.3600,
        },
        "memoryvector" => ImageCrop {
            left: 0.0200,
            top: 0.3400,
            right: 0.9800,
            bottom: 0.8300,
        },
        "memorygraph" => ImageCrop {
            left: 0.0200,
            top: 0.8200,
            right: 0.9800,
            bottom: 0.9850,
        },
        "screencapture" => ImageCrop {
            left: 0.0000,
            top: 0.0000,
            right: 1.0000,
            bottom: 0.2050,
        },
        "camera" => ImageCrop {
            left: 0.0000,
            top: 0.2050,
            right: 1.0000,
            bottom: 0.4050,
        },
        "cameradependencies" => ImageCrop {
            left: 0.0000,
            top: 0.3900,
            right: 1.0000,
            bottom: 1.0000,
        },
        _ => ImageCrop::FULL,
    }
}

pub fn steps(page_id: &str) -> &'static [FocusStep] {
    match page_id {
        "preset" => &[
            FocusStep {
                number: 1,
                rect: FocusRect {
                    left: 0.2120,
                    top: 0.3680,
                    width: 0.1220,
                    height: 0.0450,
                },
            },
            FocusStep {
                number: 2,
                rect: FocusRect {
                    left: 0.6560,
                    top: 0.5620,
                    width: 0.2730,
                    height: 0.0390,
                },
            },
            FocusStep {
                number: 3,
                rect: FocusRect {
                    left: 0.6560,
                    top: 0.7450,
                    width: 0.2480,
                    height: 0.0370,
                },
            },
            FocusStep {
                number: 4,
                rect: FocusRect {
                    left: 0.8160,
                    top: 0.7180,
                    width: 0.0840,
                    height: 0.0240,
                },
            },
            FocusStep {
                number: 5,
                rect: FocusRect {
                    left: 0.7440,
                    top: 0.3680,
                    width: 0.0990,
                    height: 0.0450,
                },
            },
            FocusStep {
                number: 6,
                rect: FocusRect {
                    left: 0.8460,
                    top: 0.3680,
                    width: 0.1020,
                    height: 0.0450,
                },
            },
        ],
        "voiceover" => &[
            FocusStep {
                number: 1,
                rect: FocusRect {
                    left: 0.0544,
                    top: 0.2205,
                    width: 0.0340,
                    height: 0.0261,
                },
            },
            FocusStep {
                number: 2,
                rect: FocusRect {
                    left: 0.2633,
                    top: 0.2227,
                    width: 0.0578,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 3,
                rect: FocusRect {
                    left: 0.3197,
                    top: 0.2227,
                    width: 0.0680,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 4,
                rect: FocusRect {
                    left: 0.4065,
                    top: 0.2227,
                    width: 0.0442,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 5,
                rect: FocusRect {
                    left: 0.5293,
                    top: 0.3295,
                    width: 0.0272,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 6,
                rect: FocusRect {
                    left: 0.1088,
                    top: 0.3295,
                    width: 0.4082,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 7,
                rect: FocusRect {
                    left: 0.1088,
                    top: 0.3614,
                    width: 0.4082,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 8,
                rect: FocusRect {
                    left: 0.1156,
                    top: 0.6003,
                    width: 0.3673,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 9,
                rect: FocusRect {
                    left: 0.4884,
                    top: 0.5963,
                    width: 0.0272,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 10,
                rect: FocusRect {
                    left: 0.5327,
                    top: 0.6352,
                    width: 0.0204,
                    height: 0.0174,
                },
            },
            FocusStep {
                number: 11,
                rect: FocusRect {
                    left: 0.1156,
                    top: 0.6323,
                    width: 0.3673,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 12,
                rect: FocusRect {
                    left: 0.1088,
                    top: 0.6641,
                    width: 0.4082,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 13,
                rect: FocusRect {
                    left: 0.1558,
                    top: 0.7883,
                    width: 0.0612,
                    height: 0.0522,
                },
            },
            FocusStep {
                number: 14,
                rect: FocusRect {
                    left: 0.4517,
                    top: 0.9014,
                    width: 0.1497,
                    height: 0.0116,
                },
            },
            FocusStep {
                number: 15,
                rect: FocusRect {
                    left: 0.1116,
                    top: 0.8974,
                    width: 0.3401,
                    height: 0.0232,
                },
            },
            FocusStep {
                number: 16,
                rect: FocusRect {
                    left: 0.4293,
                    top: 0.9432,
                    width: 0.1224,
                    height: 0.0203,
                },
            },
        ],
        "microphone" => &[
            FocusStep {
                number: 1,
                rect: FocusRect {
                    left: 0.1000,
                    top: 0.4150,
                    width: 0.1050,
                    height: 0.0350,
                },
            },
            FocusStep {
                number: 2,
                rect: FocusRect {
                    left: 0.0550,
                    top: 0.3000,
                    width: 0.0300,
                    height: 0.0280,
                },
            },
            FocusStep {
                number: 3,
                rect: FocusRect {
                    left: 0.3900,
                    top: 0.4150,
                    width: 0.3750,
                    height: 0.0350,
                },
            },
            FocusStep {
                number: 4,
                rect: FocusRect {
                    left: 0.0300,
                    top: 0.4900,
                    width: 0.4600,
                    height: 0.4800,
                },
            },
            FocusStep {
                number: 5,
                rect: FocusRect {
                    left: 0.5100,
                    top: 0.4900,
                    width: 0.4600,
                    height: 0.4800,
                },
            },
        ],
        "memoryrag" => &[
            FocusStep {
                number: 1,
                rect: FocusRect {
                    left: 0.0300,
                    top: 0.1300,
                    width: 0.2400,
                    height: 0.1000,
                },
            },
            FocusStep {
                number: 2,
                rect: FocusRect {
                    left: 0.0500,
                    top: 0.2200,
                    width: 0.8800,
                    height: 0.1000,
                },
            },
            FocusStep {
                number: 3,
                rect: FocusRect {
                    left: 0.0500,
                    top: 0.3100,
                    width: 0.3000,
                    height: 0.1000,
                },
            },
            FocusStep {
                number: 4,
                rect: FocusRect {
                    left: 0.0300,
                    top: 0.4200,
                    width: 0.2600,
                    height: 0.1000,
                },
            },
            FocusStep {
                number: 5,
                rect: FocusRect {
                    left: 0.8900,
                    top: 0.5200,
                    width: 0.0800,
                    height: 0.1000,
                },
            },
            FocusStep {
                number: 6,
                rect: FocusRect {
                    left: 0.8900,
                    top: 0.6200,
                    width: 0.0800,
                    height: 0.1000,
                },
            },
            FocusStep {
                number: 7,
                rect: FocusRect {
                    left: 0.8900,
                    top: 0.7200,
                    width: 0.0800,
                    height: 0.1000,
                },
            },
        ],
        "memoryvector" => &[
            FocusStep {
                number: 8,
                rect: FocusRect {
                    left: 0.0300,
                    top: 0.0300,
                    width: 0.2800,
                    height: 0.0900,
                },
            },
            FocusStep {
                number: 9,
                rect: FocusRect {
                    left: 0.8900,
                    top: 0.0800,
                    width: 0.0800,
                    height: 0.0900,
                },
            },
            FocusStep {
                number: 10,
                rect: FocusRect {
                    left: 0.0700,
                    top: 0.1400,
                    width: 0.7900,
                    height: 0.0800,
                },
            },
            FocusStep {
                number: 11,
                rect: FocusRect {
                    left: 0.0600,
                    top: 0.3900,
                    width: 0.8600,
                    height: 0.0800,
                },
            },
            FocusStep {
                number: 12,
                rect: FocusRect {
                    left: 0.0800,
                    top: 0.7800,
                    width: 0.1800,
                    height: 0.0700,
                },
            },
            FocusStep {
                number: 13,
                rect: FocusRect {
                    left: 0.0400,
                    top: 0.7700,
                    width: 0.1200,
                    height: 0.0800,
                },
            },
            FocusStep {
                number: 14,
                rect: FocusRect {
                    left: 0.5200,
                    top: 0.8600,
                    width: 0.4300,
                    height: 0.0700,
                },
            },
            FocusStep {
                number: 15,
                rect: FocusRect {
                    left: 0.0500,
                    top: 0.8600,
                    width: 0.4500,
                    height: 0.0700,
                },
            },
        ],
        "memorygraph" => &[
            FocusStep {
                number: 16,
                rect: FocusRect {
                    left: 0.0300,
                    top: 0.2900,
                    width: 0.3200,
                    height: 0.1600,
                },
            },
            FocusStep {
                number: 17,
                rect: FocusRect {
                    left: 0.8800,
                    top: 0.4300,
                    width: 0.0800,
                    height: 0.1500,
                },
            },
            FocusStep {
                number: 18,
                rect: FocusRect {
                    left: 0.8800,
                    top: 0.6200,
                    width: 0.0800,
                    height: 0.1500,
                },
            },
        ],
        "screencapture" => &[
            FocusStep {
                number: 1,
                rect: FocusRect {
                    left: 0.0400,
                    top: 0.2900,
                    width: 0.3900,
                    height: 0.1200,
                },
            },
            FocusStep {
                number: 2,
                rect: FocusRect {
                    left: 0.9000,
                    top: 0.4000,
                    width: 0.0800,
                    height: 0.1500,
                },
            },
        ],
        "models" => &[
            FocusStep {
                number: 1,
                rect: FocusRect {
                    left: 0.4880,
                    top: 0.4150,
                    width: 0.1780,
                    height: 0.0590,
                },
            },
            FocusStep {
                number: 2,
                rect: FocusRect {
                    left: 0.3670,
                    top: 0.6060,
                    width: 0.2100,
                    height: 0.0400,
                },
            },
            FocusStep {
                number: 3,
                rect: FocusRect {
                    left: 0.3670,
                    top: 0.6790,
                    width: 0.2100,
                    height: 0.0400,
                },
            },
            FocusStep {
                number: 4,
                rect: FocusRect {
                    left: 0.3670,
                    top: 0.7500,
                    width: 0.2100,
                    height: 0.0400,
                },
            },
            FocusStep {
                number: 5,
                rect: FocusRect {
                    left: 0.3670,
                    top: 0.8210,
                    width: 0.2100,
                    height: 0.0400,
                },
            },
            FocusStep {
                number: 6,
                rect: FocusRect {
                    left: 0.3670,
                    top: 0.8930,
                    width: 0.2100,
                    height: 0.0400,
                },
            },
            FocusStep {
                number: 7,
                rect: FocusRect {
                    left: 0.3670,
                    top: 0.9620,
                    width: 0.2100,
                    height: 0.0360,
                },
            },
        ],
        "camera" => &[
            FocusStep {
                number: 3,
                rect: FocusRect {
                    left: 0.0400,
                    top: 0.5000,
                    width: 0.3900,
                    height: 0.1200,
                },
            },
            FocusStep {
                number: 4,
                rect: FocusRect {
                    left: 0.9000,
                    top: 0.6300,
                    width: 0.0800,
                    height: 0.1200,
                },
            },
            FocusStep {
                number: 9,
                rect: FocusRect {
                    left: 0.1700,
                    top: 0.7200,
                    width: 0.7700,
                    height: 0.1000,
                },
            },
        ],
        "cameradependencies" => &[
            FocusStep {
                number: 5,
                rect: FocusRect {
                    left: 0.8800,
                    top: 0.2600,
                    width: 0.0800,
                    height: 0.1000,
                },
            },
            FocusStep {
                number: 6,
                rect: FocusRect {
                    left: 0.3900,
                    top: 0.0800,
                    width: 0.2000,
                    height: 0.0900,
                },
            },
            FocusStep {
                number: 7,
                rect: FocusRect {
                    left: 0.0200,
                    top: 0.7300,
                    width: 0.1900,
                    height: 0.0900,
                },
            },
            FocusStep {
                number: 8,
                rect: FocusRect {
                    left: 0.8200,
                    top: 0.6400,
                    width: 0.1500,
                    height: 0.0900,
                },
            },
        ],
        "final" => &[
            FocusStep {
                number: 1,
                rect: FocusRect {
                    left: 0.9240,
                    top: 0.1000,
                    width: 0.0360,
                    height: 0.0500,
                },
            },
            FocusStep {
                number: 2,
                rect: FocusRect {
                    left: 0.7900,
                    top: 0.7580,
                    width: 0.1350,
                    height: 0.0450,
                },
            },
            FocusStep {
                number: 3,
                rect: FocusRect {
                    left: 0.9380,
                    top: 0.7500,
                    width: 0.0280,
                    height: 0.0450,
                },
            },
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_python_focus_step_stays_bounded_and_unique() {
        let mut nonempty = 0;
        for page in super::super::PAGES {
            let mut numbers = HashSet::new();
            let crop = crop(page.id);
            assert!(crop.left >= 0.0 && crop.top >= 0.0);
            assert!(crop.left < crop.right && crop.right <= 1.0);
            assert!(crop.top < crop.bottom && crop.bottom <= 1.0);
            for step in steps(page.id) {
                nonempty += 1;
                assert!(
                    numbers.insert(step.number),
                    "duplicate focus step for {}",
                    page.id
                );
                let rect = step.rect;
                assert!(rect.left >= 0.0 && rect.top >= 0.0);
                assert!(rect.width > 0.0 && rect.height > 0.0);
                assert!(rect.left + rect.width <= 1.00001);
                assert!(rect.top + rect.height <= 1.00001);
            }
        }
        assert_eq!(nonempty, 64);
    }

    #[test]
    fn python_focus_geometry_covers_original_controls() {
        assert_eq!(steps("preset").len(), 6);
        assert_eq!(steps("voiceover").len(), 16);
        assert_eq!(steps("sandbox"), &[]);
        assert_eq!(steps("memoryvector")[0].number, 8);
        assert_eq!(steps("final")[2].number, 3);
        assert_eq!(crop("screencapture").bottom, 0.205);
    }
}
