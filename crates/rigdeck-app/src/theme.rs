//! Application themes. Each theme is a small palette applied to egui's visuals and to the
//! spectrum (plot background, grid, trace, accent).

use eframe::egui::{self, Color32, Stroke, Theme, ThemePreference, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppTheme {
    Dark,
    Midnight,
    Light,
    HighContrast,
    Graphite,
    Nord,
    Dracula,
    SolarizedDark,
    SolarizedLight,
    Gruvbox,
    Monokai,
    Ocean,
    Forest,
    NightRed,
    Amber,
    Paper,
}

struct Palette {
    name: &'static str,
    dark: bool,
    panel: [u8; 3],
    window: [u8; 3],
    extreme: [u8; 3],
    widget: [u8; 3],
    widget_hover: [u8; 3],
    text: Option<[u8; 3]>,
    accent: [u8; 3],
    plot: [u8; 3],
    grid: [u8; 3],
    trace: [u8; 3],
    scale: [u8; 3],
}

const fn c(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

impl AppTheme {
    pub const ALL: [AppTheme; 16] = [
        AppTheme::Dark,
        AppTheme::Midnight,
        AppTheme::Graphite,
        AppTheme::Nord,
        AppTheme::Dracula,
        AppTheme::SolarizedDark,
        AppTheme::Gruvbox,
        AppTheme::Monokai,
        AppTheme::Ocean,
        AppTheme::Forest,
        AppTheme::NightRed,
        AppTheme::Amber,
        AppTheme::HighContrast,
        AppTheme::Light,
        AppTheme::SolarizedLight,
        AppTheme::Paper,
    ];

    fn palette(self) -> Palette {
        match self {
            AppTheme::Dark => Palette {
                name: "Dark",
                dark: true,
                panel: [24, 25, 28],
                window: [30, 31, 35],
                extreme: [14, 15, 17],
                widget: [45, 47, 52],
                widget_hover: [62, 65, 72],
                text: None,
                accent: [70, 140, 255],
                plot: [10, 11, 14],
                grid: [38, 38, 38],
                trace: [150, 200, 255],
                scale: [185, 185, 185],
            },
            AppTheme::Midnight => Palette {
                name: "Midnight blue",
                dark: true,
                panel: [16, 30, 48],
                window: [20, 36, 56],
                extreme: [8, 18, 30],
                widget: [30, 52, 78],
                widget_hover: [40, 70, 104],
                text: None,
                accent: [60, 200, 230],
                plot: [8, 22, 38],
                grid: [30, 55, 80],
                trace: [225, 235, 245],
                scale: [170, 195, 215],
            },
            AppTheme::Graphite => Palette {
                name: "Graphite",
                dark: true,
                panel: [44, 46, 50],
                window: [52, 54, 58],
                extreme: [30, 31, 34],
                widget: [64, 67, 72],
                widget_hover: [80, 84, 90],
                text: Some([225, 225, 228]),
                accent: [255, 149, 0],
                plot: [28, 29, 31],
                grid: [55, 57, 61],
                trace: [240, 240, 240],
                scale: [190, 190, 195],
            },
            AppTheme::Nord => Palette {
                name: "Nord",
                dark: true,
                panel: [46, 52, 64],
                window: [59, 66, 82],
                extreme: [36, 41, 51],
                widget: [67, 76, 94],
                widget_hover: [76, 86, 106],
                text: Some([216, 222, 233]),
                accent: [136, 192, 208],
                plot: [36, 41, 51],
                grid: [59, 66, 82],
                trace: [143, 188, 187],
                scale: [216, 222, 233],
            },
            AppTheme::Dracula => Palette {
                name: "Dracula",
                dark: true,
                panel: [40, 42, 54],
                window: [48, 50, 64],
                extreme: [33, 34, 44],
                widget: [68, 71, 90],
                widget_hover: [86, 90, 112],
                text: Some([248, 248, 242]),
                accent: [189, 147, 249],
                plot: [30, 31, 41],
                grid: [56, 58, 74],
                trace: [139, 233, 253],
                scale: [200, 200, 210],
            },
            AppTheme::SolarizedDark => Palette {
                name: "Solarized dark",
                dark: true,
                panel: [0, 43, 54],
                window: [7, 54, 66],
                extreme: [0, 33, 43],
                widget: [7, 54, 66],
                widget_hover: [24, 76, 90],
                text: Some([147, 161, 161]),
                accent: [38, 139, 210],
                plot: [0, 30, 38],
                grid: [7, 54, 66],
                trace: [181, 137, 0],
                scale: [131, 148, 150],
            },
            AppTheme::Gruvbox => Palette {
                name: "Gruvbox",
                dark: true,
                panel: [40, 40, 40],
                window: [50, 48, 47],
                extreme: [29, 32, 33],
                widget: [60, 56, 54],
                widget_hover: [80, 73, 69],
                text: Some([235, 219, 178]),
                accent: [250, 189, 47],
                plot: [29, 32, 33],
                grid: [60, 56, 54],
                trace: [184, 187, 38],
                scale: [213, 196, 161],
            },
            AppTheme::Monokai => Palette {
                name: "Monokai",
                dark: true,
                panel: [39, 40, 34],
                window: [49, 50, 43],
                extreme: [30, 31, 26],
                widget: [62, 61, 50],
                widget_hover: [80, 79, 66],
                text: Some([248, 248, 242]),
                accent: [166, 226, 46],
                plot: [30, 31, 26],
                grid: [62, 61, 50],
                trace: [102, 217, 239],
                scale: [200, 200, 190],
            },
            AppTheme::Ocean => Palette {
                name: "Ocean",
                dark: true,
                panel: [15, 39, 52],
                window: [20, 50, 66],
                extreme: [8, 26, 36],
                widget: [26, 66, 86],
                widget_hover: [34, 86, 110],
                text: Some([210, 235, 240]),
                accent: [0, 210, 170],
                plot: [6, 22, 32],
                grid: [20, 56, 72],
                trace: [120, 230, 220],
                scale: [170, 210, 220],
            },
            AppTheme::Forest => Palette {
                name: "Forest",
                dark: true,
                panel: [24, 36, 28],
                window: [30, 45, 35],
                extreme: [15, 24, 18],
                widget: [40, 60, 46],
                widget_hover: [52, 78, 60],
                text: Some([220, 235, 215]),
                accent: [130, 210, 100],
                plot: [12, 20, 15],
                grid: [36, 54, 40],
                trace: [190, 235, 160],
                scale: [180, 205, 175],
            },
            AppTheme::NightRed => Palette {
                name: "Night (red, keeps night vision)",
                dark: true,
                panel: [12, 0, 0],
                window: [18, 0, 0],
                extreme: [6, 0, 0],
                widget: [40, 4, 4],
                widget_hover: [60, 8, 8],
                text: Some([210, 40, 40]),
                accent: [255, 50, 50],
                plot: [4, 0, 0],
                grid: [40, 6, 6],
                trace: [230, 50, 40],
                scale: [180, 40, 40],
            },
            AppTheme::Amber => Palette {
                name: "Amber terminal",
                dark: true,
                panel: [20, 14, 4],
                window: [28, 20, 6],
                extreme: [12, 8, 2],
                widget: [52, 36, 8],
                widget_hover: [72, 50, 12],
                text: Some([255, 176, 0]),
                accent: [255, 200, 60],
                plot: [10, 7, 2],
                grid: [50, 34, 8],
                trace: [255, 190, 40],
                scale: [220, 150, 20],
            },
            AppTheme::HighContrast => Palette {
                name: "High contrast",
                dark: true,
                panel: [0, 0, 0],
                window: [0, 0, 0],
                extreme: [0, 0, 0],
                widget: [30, 30, 30],
                widget_hover: [60, 60, 60],
                text: Some([255, 255, 255]),
                accent: [255, 220, 0],
                plot: [0, 0, 0],
                grid: [90, 90, 90],
                trace: [255, 255, 255],
                scale: [230, 230, 230],
            },
            AppTheme::Light => Palette {
                name: "Light",
                dark: false,
                panel: [248, 248, 248],
                window: [255, 255, 255],
                extreme: [255, 255, 255],
                widget: [230, 230, 230],
                widget_hover: [215, 215, 215],
                text: None,
                accent: [20, 100, 210],
                plot: [245, 247, 250],
                grid: [205, 205, 205],
                trace: [20, 70, 160],
                scale: [70, 70, 70],
            },
            AppTheme::SolarizedLight => Palette {
                name: "Solarized light",
                dark: false,
                panel: [253, 246, 227],
                window: [238, 232, 213],
                extreme: [255, 251, 240],
                widget: [238, 232, 213],
                widget_hover: [222, 214, 190],
                text: Some([88, 110, 117]),
                accent: [38, 139, 210],
                plot: [253, 246, 227],
                grid: [225, 218, 196],
                trace: [203, 75, 22],
                scale: [101, 123, 131],
            },
            AppTheme::Paper => Palette {
                name: "Paper",
                dark: false,
                panel: [244, 241, 234],
                window: [250, 248, 243],
                extreme: [255, 255, 255],
                widget: [228, 224, 214],
                widget_hover: [214, 208, 194],
                text: Some([40, 40, 40]),
                accent: [176, 60, 40],
                plot: [250, 248, 243],
                grid: [220, 214, 200],
                trace: [40, 40, 40],
                scale: [80, 80, 80],
            },
        }
    }

    pub fn name(self) -> &'static str {
        self.palette().name
    }

    /// Accent used for the tuned-frequency marker and highlights.
    pub fn accent(self) -> Color32 {
        c(self.palette().accent)
    }

    /// Background of the spectrum and waterfall area.
    pub fn plot_bg(self) -> Color32 {
        c(self.palette().plot)
    }

    pub fn grid(self) -> Color32 {
        c(self.palette().grid)
    }

    pub fn trace(self) -> Color32 {
        c(self.palette().trace)
    }

    pub fn scale_text(self) -> Color32 {
        c(self.palette().scale)
    }

    pub fn apply(self, ctx: &egui::Context) {
        let p = self.palette();
        let accent = c(p.accent);
        let (pref, theme, mut v) = if p.dark {
            (ThemePreference::Dark, Theme::Dark, Visuals::dark())
        } else {
            (ThemePreference::Light, Theme::Light, Visuals::light())
        };
        v.panel_fill = c(p.panel);
        v.window_fill = c(p.window);
        v.extreme_bg_color = c(p.extreme);
        v.faint_bg_color = c(p.window);
        v.override_text_color = p.text.map(c);
        for w in [&mut v.widgets.inactive, &mut v.widgets.noninteractive] {
            w.weak_bg_fill = c(p.widget);
            w.bg_fill = c(p.widget);
        }
        v.widgets.noninteractive.weak_bg_fill = c(p.panel);
        v.widgets.noninteractive.bg_fill = c(p.panel);
        v.widgets.hovered.weak_bg_fill = c(p.widget_hover);
        v.widgets.hovered.bg_fill = c(p.widget_hover);
        v.widgets.active.weak_bg_fill = accent.gamma_multiply(0.7);
        v.widgets.active.bg_fill = accent.gamma_multiply(0.7);
        if let Some(t) = p.text {
            let t = c(t);
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t);
            v.widgets.inactive.fg_stroke = Stroke::new(1.0, t);
        }
        if self == AppTheme::HighContrast {
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(140));
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::WHITE);
        }
        let strong_selection = matches!(self, AppTheme::HighContrast | AppTheme::Amber | AppTheme::NightRed);
        v.selection.bg_fill = accent.gamma_multiply(if strong_selection { 0.9 } else { 0.55 });
        v.selection.stroke = Stroke::new(1.0, if strong_selection || !p.dark { Color32::BLACK } else { Color32::WHITE });
        v.hyperlink_color = accent;
        ctx.set_theme(pref);
        ctx.set_visuals_of(theme, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_has_a_unique_name_and_readable_trace() {
        let mut names: Vec<&str> = AppTheme::ALL.iter().map(|t| t.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), AppTheme::ALL.len());
        let lum = |c: Color32| f32::from(c.r()) * 0.3 + f32::from(c.g()) * 0.59 + f32::from(c.b()) * 0.11;
        for t in AppTheme::ALL {
            let contrast = (lum(t.trace()) - lum(t.plot_bg())).abs();
            assert!(contrast > 60.0, "{} trace too close to its background", t.name());
        }
    }
}
