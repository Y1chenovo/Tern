//! Visual theme: Apple-style system colours (light / dark), system fonts and
//! terminal palettes.
//!
//! Fonts: one CJK-capable system face (Microsoft YaHei UI / PingFang / Noto Sans CJK)
//! is the *primary* proportional font so Latin and Chinese share one baseline —
//! mixing fonts per glyph is what made labels look vertically misaligned.

use super::term::TermPalette;
use crate::profiles::ThemePref;
use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, TextStyle};

/// Font family used for semibold text (headings, emphasised labels).
pub const SEMIBOLD: &str = "semibold";
/// Bold face of the terminal font (only when one with the same advance exists).
pub const TERM_BOLD: &str = "term-bold";

/// Semantic colours shared by every custom-drawn widget.
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub dark: bool,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub separator: Color32,
    /// Main content background.
    pub bg: Color32,
    /// Toolbar / sidebar / status bar ("window chrome").
    pub chrome: Color32,
    /// Raised surfaces: cards, popovers, active tab.
    pub card: Color32,
    /// Neutral control fill (text fields, secondary buttons, segmented track).
    pub control: Color32,
    pub control_hover: Color32,
    pub control_press: Color32,
    pub row_hover: Color32,
    pub selected: Color32,
    pub shadow: Color32,
}

pub fn tokens(theme: ThemePref) -> Tokens {
    let c = Color32::from_rgb;
    match theme {
        ThemePref::Light => Tokens {
            dark: false,
            accent: c(0, 122, 255),
            accent_soft: c(224, 237, 255),
            ok: c(40, 167, 69),
            warn: c(230, 136, 0),
            err: c(255, 59, 48),
            text: c(29, 29, 31),
            text2: c(110, 110, 115),
            text3: c(161, 161, 166),
            separator: c(226, 226, 230),
            bg: c(255, 255, 255),
            chrome: c(246, 246, 248),
            card: c(255, 255, 255),
            control: c(236, 236, 240),
            control_hover: c(227, 227, 232),
            control_press: c(214, 214, 220),
            row_hover: c(236, 236, 240),
            selected: c(220, 233, 255),
            shadow: Color32::from_black_alpha(26),
        },
        ThemePref::Dark => Tokens {
            dark: true,
            accent: c(10, 132, 255),
            accent_soft: c(22, 52, 92),
            ok: c(48, 209, 88),
            warn: c(255, 159, 10),
            err: c(255, 69, 58),
            text: c(245, 245, 247),
            text2: c(152, 152, 157),
            text3: c(99, 99, 104),
            separator: c(46, 46, 50),
            bg: c(28, 28, 30),
            chrome: c(36, 36, 38),
            card: c(44, 44, 46),
            control: c(58, 58, 60),
            control_hover: c(72, 72, 74),
            control_press: c(86, 86, 90),
            row_hover: c(50, 50, 53),
            selected: c(30, 64, 110),
            shadow: Color32::from_black_alpha(90),
        },
    }
}

pub fn term_palette(theme: ThemePref) -> TermPalette {
    let c = Color32::from_rgb;
    match theme {
        ThemePref::Dark => TermPalette {
            fg: c(229, 229, 234),
            bg: c(22, 22, 24),
            cursor: c(10, 132, 255),
            selection: c(38, 72, 120),
            ansi: [
                c(52, 52, 56),
                c(255, 99, 89),
                c(72, 214, 104),
                c(255, 196, 64),
                c(64, 156, 255),
                c(208, 128, 255),
                c(90, 210, 230),
                c(206, 206, 212),
                c(110, 110, 118),
                c(255, 128, 120),
                c(110, 230, 140),
                c(255, 214, 110),
                c(112, 180, 255),
                c(222, 160, 255),
                c(130, 226, 240),
                c(250, 250, 252),
            ],
        },
        ThemePref::Light => TermPalette {
            // Soft paper tone instead of pure white to avoid glare.
            fg: c(36, 38, 42),
            bg: c(246, 246, 243),
            cursor: c(0, 110, 230),
            selection: c(204, 222, 248),
            ansi: [
                c(29, 29, 31),
                c(215, 44, 36),
                c(30, 140, 60),
                c(178, 116, 0),
                c(0, 100, 220),
                c(150, 56, 190),
                c(0, 132, 150),
                c(142, 142, 147),
                c(99, 99, 104),
                c(240, 64, 54),
                c(46, 164, 82),
                c(200, 140, 0),
                c(24, 120, 245),
                c(176, 82, 214),
                c(16, 156, 176),
                c(60, 60, 66),
            ],
        },
    }
}

fn visuals(t: &Tokens) -> egui::Visuals {
    let mut v = if t.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.override_text_color = Some(t.text);
    v.panel_fill = t.bg;
    v.window_fill = t.card;
    v.faint_bg_color = t.chrome;
    v.extreme_bg_color = t.control;
    v.code_bg_color = t.control;
    v.window_stroke = egui::Stroke::new(1.0_f32, t.separator);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = egui::Shadow {
        offset: [0, 12],
        blur: 40,
        spread: 0,
        color: Color32::from_black_alpha(if t.dark { 140 } else { 46 }),
    };
    v.popup_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha(if t.dark { 110 } else { 34 }),
    };
    v.selection.bg_fill = if t.dark {
        Color32::from_rgba_unmultiplied(10, 132, 255, 110)
    } else {
        Color32::from_rgba_unmultiplied(0, 122, 255, 70)
    };
    v.selection.stroke = egui::Stroke::new(1.0_f32, t.accent);
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.err;
    v.text_cursor.stroke = egui::Stroke::new(2.0_f32, t.accent);
    v.striped = false;
    v.slider_trailing_fill = true;
    v.indent_has_left_vline = false;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.card;
    w.noninteractive.weak_bg_fill = t.card;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, t.separator);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, t.text);

    w.inactive.bg_fill = t.control;
    w.inactive.weak_bg_fill = t.control;
    w.inactive.bg_stroke = egui::Stroke::NONE;
    w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, t.text);

    w.hovered.bg_fill = t.control_hover;
    w.hovered.weak_bg_fill = t.control_hover;
    w.hovered.bg_stroke = egui::Stroke::NONE;
    w.hovered.fg_stroke = egui::Stroke::new(1.5_f32, t.text);

    w.active.bg_fill = t.control_press;
    w.active.weak_bg_fill = t.control_press;
    w.active.bg_stroke = egui::Stroke::NONE;
    w.active.fg_stroke = egui::Stroke::new(1.5_f32, t.text);

    w.open.bg_fill = t.control_hover;
    w.open.weak_bg_fill = t.control_hover;
    w.open.bg_stroke = egui::Stroke::NONE;
    w.open.fg_stroke = egui::Stroke::new(1.0_f32, t.text);

    for ws in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        ws.corner_radius = CornerRadius::same(7);
        ws.expansion = 0.0;
    }
    v
}

/// Apply visuals + spacing + text styles, and publish the tokens for widgets.
pub fn apply(ctx: &egui::Context, theme: ThemePref) {
    let t = tokens(theme);
    let mut style = (*ctx.style()).clone();
    style.visuals = visuals(&t);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.window_margin = Margin::same(20);
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.menu_spacing = 2.0;
    style.spacing.indent = 14.0;
    style.spacing.interact_size = egui::vec2(28.0, 28.0);
    style.spacing.combo_height = 280.0;
    style.spacing.icon_width = 16.0;
    style.spacing.icon_width_inner = 8.0;
    style.spacing.scroll = egui::style::ScrollStyle::floating();
    style.interaction.tooltip_delay = 0.45;
    style.interaction.selectable_labels = false;
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(17.0, FontFamily::Name(SEMIBOLD.into())),
        ),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();
    // egui keeps separate dark / light styles and follows the OS theme by default.
    // Pin the theme and install the same style for both, otherwise popups, menus and
    // text fields pick up egui's stock style when the OS theme differs (white menus,
    // invisible text in dark mode on a light Windows desktop).
    ctx.set_theme(if t.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.data_mut(|d| d.insert_temp(egui::Id::NULL, t));
}

/// Tokens published by [`apply`].
pub fn tok(ctx: &egui::Context) -> Tokens {
    ctx.data(|d| d.get_temp::<Tokens>(egui::Id::NULL))
        .unwrap_or_else(|| tokens(ThemePref::Light))
}

fn read_first(cands: &[(std::path::PathBuf, u32)]) -> Option<(String, u32, Vec<u8>)> {
    cands.iter().find_map(|(p, i)| {
        std::fs::read(p)
            .ok()
            .map(|b| (p.display().to_string(), *i, b))
    })
}

fn font_data(bytes: Vec<u8>, index: u32) -> std::sync::Arc<egui::FontData> {
    std::sync::Arc::new(egui::FontData {
        font: std::borrow::Cow::Owned(bytes),
        index,
        tweak: Default::default(),
    })
}

/// Install system UI / semibold / monospace fonts. Returns the UI font path in use.
pub fn install_fonts(ctx: &egui::Context) -> Option<String> {
    type Cands = Vec<(std::path::PathBuf, u32)>;
    #[allow(unused_mut)]
    let (mut regular, mut semibold, mut mono): (Cands, Cands, Cands) = (vec![], vec![], vec![]);
    // Bold companions of monospace faces: (regular file, its bold face).
    #[allow(unused_mut)]
    let mut mono_bold: Vec<(std::path::PathBuf, (std::path::PathBuf, u32))> = vec![];

    #[cfg(windows)]
    {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let f = std::path::Path::new(&windir).join("Fonts");
        // msyh.ttc: 0 = Microsoft YaHei, 1 = Microsoft YaHei UI (tighter UI metrics)
        regular.extend([
            (f.join("msyh.ttc"), 1),
            (f.join("msyh.ttf"), 0),
            (f.join("Deng.ttf"), 0),
            (f.join("simhei.ttf"), 0),
        ]);
        semibold.extend([
            (f.join("msyhbd.ttc"), 1),
            (f.join("msyhbd.ttf"), 0),
            (f.join("Dengb.ttf"), 0),
        ]);
        mono.extend([(f.join("CascadiaMono.ttf"), 0), (f.join("consola.ttf"), 0)]);
        mono_bold.push((f.join("consola.ttf"), (f.join("consolab.ttf"), 0)));
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let lf = std::path::Path::new(&local)
                .join("Microsoft")
                .join("Windows")
                .join("Fonts");
            mono.insert(0, (lf.join("CascadiaMono.ttf"), 0));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Hiragino Sans GB.ttc: 0 = W3 (regular), 1 = W6 (semibold)
        regular.push(("/System/Library/Fonts/Hiragino Sans GB.ttc".into(), 0));
        regular.push(("/System/Library/Fonts/STHeiti Light.ttc".into(), 0));
        semibold.push(("/System/Library/Fonts/Hiragino Sans GB.ttc".into(), 1));
        semibold.push(("/System/Library/Fonts/STHeiti Medium.ttc".into(), 0));
        for p in [
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Menlo.ttc",
        ] {
            mono.push((p.into(), 0));
        }
        // Menlo.ttc: 0 = Regular, 1 = Bold
        mono_bold.push((
            "/System/Library/Fonts/Menlo.ttc".into(),
            ("/System/Library/Fonts/Menlo.ttc".into(), 1),
        ));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Noto Sans CJK .ttc order: JP, KR, SC, TC, HK → 2 = Simplified Chinese.
        for dir in [
            "/usr/share/fonts/opentype/noto",
            "/usr/share/fonts/noto-cjk",
            "/usr/share/fonts/google-noto-cjk",
        ] {
            regular.push((format!("{dir}/NotoSansCJK-Regular.ttc").into(), 2));
            semibold.push((format!("{dir}/NotoSansCJK-Medium.ttc").into(), 2));
            semibold.push((format!("{dir}/NotoSansCJK-Bold.ttc").into(), 2));
        }
        for p in [
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ] {
            regular.push((p.into(), 0));
        }
    }

    let mut defs = egui::FontDefinitions::default();
    let mut used = None;
    if let Some((path, idx, bytes)) = read_first(&regular) {
        defs.font_data.insert("ui".into(), font_data(bytes, idx));
        // Primary for proportional text; fallback (after Hack) for terminal CJK.
        defs.families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "ui".into());
        defs.families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("ui".into());
        used = Some(path);
    }
    let mut semi_family: Vec<String> = Vec::new();
    if let Some((_, idx, bytes)) = read_first(&semibold) {
        defs.font_data
            .insert("ui-semibold".into(), font_data(bytes, idx));
        semi_family.push("ui-semibold".into());
    }
    // Fallbacks: regular UI face, then egui defaults (symbols / emoji).
    semi_family.extend(
        defs.families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), semi_family);

    if let Some((path, idx, bytes)) = read_first(&mono) {
        defs.font_data.insert("mono".into(), font_data(bytes, idx));
        defs.families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "mono".into());
        // Its bold face, if it has one: same fallbacks as the regular family.
        let bold = mono_bold
            .iter()
            .find(|(regular, _)| *regular == path)
            .and_then(|(_, b)| read_first(std::slice::from_ref(b)));
        if let Some((_, bidx, bbytes)) = bold {
            defs.font_data
                .insert("mono-bold".into(), font_data(bbytes, bidx));
            let mut fam = vec!["mono-bold".to_string()];
            fam.extend(
                defs.families
                    .get(&FontFamily::Monospace)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|n| n != "mono"),
            );
            defs.families
                .insert(FontFamily::Name(TERM_BOLD.into()), fam);
        }
    }
    ctx.set_fonts(defs);
    used
}

/// Match the native Windows title bar to the app: dark mode + caption / border
/// colour equal to the toolbar, so there is no bright strip above the window.
#[cfg(windows)]
pub fn style_title_bar(window: &impl raw_window_handle::HasWindowHandle, t: &Tokens) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return;
    };
    let hwnd = h.hwnd.get() as *mut core::ffi::c_void;
    let colorref = |c: Color32| (c.r() as u32) | ((c.g() as u32) << 8) | ((c.b() as u32) << 16);
    let dark: i32 = i32::from(t.dark);
    let caption: u32 = colorref(t.chrome);
    let text: u32 = colorref(t.text);
    // SAFETY: valid HWND from winit; attribute payloads are 4-byte values that
    // outlive the calls. Unsupported attributes just return an error code.
    unsafe {
        // DWMWA_USE_IMMERSIVE_DARK_MODE (20; 19 on Windows 10 before 20H1)
        DwmSetWindowAttribute(hwnd, 20, &dark as *const i32 as *const core::ffi::c_void, 4);
        DwmSetWindowAttribute(hwnd, 19, &dark as *const i32 as *const core::ffi::c_void, 4);
        // Windows 11: DWMWA_BORDER_COLOR (34), DWMWA_CAPTION_COLOR (35), DWMWA_TEXT_COLOR (36)
        DwmSetWindowAttribute(
            hwnd,
            34,
            &caption as *const u32 as *const core::ffi::c_void,
            4,
        );
        DwmSetWindowAttribute(
            hwnd,
            35,
            &caption as *const u32 as *const core::ffi::c_void,
            4,
        );
        DwmSetWindowAttribute(hwnd, 36, &text as *const u32 as *const core::ffi::c_void, 4);
    }
}

#[cfg(not(windows))]
pub fn style_title_bar<T>(_window: &T, _t: &Tokens) {}

/// Borderless window polish on Windows 11: rounded corners and a DWM shadow
/// (the frame is extended 1 px into the client area so DWM draws it).
#[cfg(windows)]
pub fn style_borderless(window: &impl raw_window_handle::HasWindowHandle) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
    use windows_sys::Win32::UI::Controls::MARGINS;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return;
    };
    let hwnd = h.hwnd.get() as *mut core::ffi::c_void;
    let round: i32 = 2; // DWMWCP_ROUND
    let margins = MARGINS {
        cxLeftWidth: 1,
        cxRightWidth: 1,
        cyTopHeight: 1,
        cyBottomHeight: 1,
    };
    // SAFETY: valid HWND from winit; payloads outlive the calls. Failures
    // (older Windows) are harmless and ignored.
    unsafe {
        // DWMWA_WINDOW_CORNER_PREFERENCE (33)
        DwmSetWindowAttribute(
            hwnd,
            33,
            &round as *const i32 as *const core::ffi::c_void,
            4,
        );
        DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
}

#[cfg(not(windows))]
pub fn style_borderless<T>(_window: &T) {}
