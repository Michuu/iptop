//! Colour themes (`c` cycles, `--theme NAME`).

use ratatui::style::Color;
use std::sync::atomic::{AtomicUsize, Ordering};

type Rgb = (u8, u8, u8);

pub struct Theme {
    pub name: &'static str,
    /// Main background; None leaves the terminal's own.
    pub bg: Option<Color>,
    pub fg: Color,
    pub dim: Color,
    pub border: Color,
    pub accent: Color,
    /// Text on accent-coloured badges.
    pub on_accent: Color,
    pub sel_bg: Color,
    /// Text of the selected row and of input.
    pub bright: Color,
    pub rx: Color,
    pub tx: Color,
    pub rx_bar: Color,
    pub tx_bar: Color,
    pub warn: Color,
    pub err: Color,
    /// low → mid → high gradients for graphs, sparklines and the map
    pub rx_grad: [Rgb; 3],
    pub tx_grad: [Rgb; 3],
    pub heat_grad: [Rgb; 3],
    /// World map land (block style); the sea is the background.
    pub land: Color,
}

const fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "tokyo-night",
        bg: None,
        fg: rgb((205, 210, 230)),
        dim: rgb((110, 115, 140)),
        border: rgb((65, 72, 104)),
        accent: rgb((122, 162, 247)),
        on_accent: Color::Black,
        sel_bg: rgb((40, 44, 66)),
        bright: rgb((255, 255, 255)),
        rx: rgb((115, 218, 140)),
        tx: rgb((187, 154, 247)),
        rx_bar: rgb((26, 72, 52)),
        tx_bar: rgb((68, 46, 102)),
        warn: rgb((224, 175, 104)),
        err: rgb((247, 118, 142)),
        rx_grad: [(80, 200, 120), (230, 200, 70), (240, 90, 90)],
        tx_grad: [(90, 180, 235), (130, 120, 245), (225, 100, 220)],
        heat_grad: [(60, 110, 200), (230, 200, 70), (245, 80, 80)],
        land: rgb((44, 50, 74)),
    },
    Theme {
        name: "dracula",
        bg: Some(rgb((40, 42, 54))),
        fg: rgb((248, 248, 242)),
        dim: rgb((130, 140, 180)),
        border: rgb((68, 71, 90)),
        accent: rgb((189, 147, 249)),
        on_accent: rgb((40, 42, 54)),
        sel_bg: rgb((58, 60, 78)),
        bright: rgb((255, 255, 255)),
        rx: rgb((80, 250, 123)),
        tx: rgb((255, 121, 198)),
        rx_bar: rgb((36, 84, 56)),
        tx_bar: rgb((92, 46, 80)),
        warn: rgb((241, 250, 140)),
        err: rgb((255, 85, 85)),
        rx_grad: [(80, 250, 123), (241, 250, 140), (255, 85, 85)],
        tx_grad: [(139, 233, 253), (189, 147, 249), (255, 121, 198)],
        heat_grad: [(98, 114, 164), (241, 250, 140), (255, 85, 85)],
        land: rgb((68, 71, 90)),
    },
    Theme {
        name: "nord",
        bg: Some(rgb((46, 52, 64))),
        fg: rgb((216, 222, 233)),
        dim: rgb((130, 142, 165)),
        border: rgb((76, 86, 106)),
        accent: rgb((136, 192, 208)),
        on_accent: rgb((46, 52, 64)),
        sel_bg: rgb((59, 66, 82)),
        bright: rgb((255, 255, 255)),
        rx: rgb((163, 190, 140)),
        tx: rgb((180, 142, 173)),
        rx_bar: rgb((62, 78, 66)),
        tx_bar: rgb((78, 66, 86)),
        warn: rgb((235, 203, 139)),
        err: rgb((191, 97, 106)),
        rx_grad: [(163, 190, 140), (235, 203, 139), (191, 97, 106)],
        tx_grad: [(129, 161, 193), (180, 142, 173), (208, 135, 112)],
        heat_grad: [(94, 129, 172), (235, 203, 139), (191, 97, 106)],
        land: rgb((67, 76, 94)),
    },
    Theme {
        name: "gruvbox",
        bg: Some(rgb((40, 40, 40))),
        fg: rgb((235, 219, 178)),
        dim: rgb((146, 131, 116)),
        border: rgb((80, 73, 69)),
        accent: rgb((250, 189, 47)),
        on_accent: rgb((40, 40, 40)),
        sel_bg: rgb((60, 56, 54)),
        bright: rgb((255, 255, 255)),
        rx: rgb((184, 187, 38)),
        tx: rgb((131, 165, 152)),
        rx_bar: rgb((66, 70, 30)),
        tx_bar: rgb((50, 66, 62)),
        warn: rgb((254, 128, 25)),
        err: rgb((251, 73, 52)),
        rx_grad: [(184, 187, 38), (250, 189, 47), (251, 73, 52)],
        tx_grad: [(131, 165, 152), (211, 134, 155), (254, 128, 25)],
        heat_grad: [(69, 133, 136), (250, 189, 47), (251, 73, 52)],
        land: rgb((80, 73, 69)),
    },
    Theme {
        name: "catppuccin",
        bg: Some(rgb((30, 30, 46))),
        fg: rgb((205, 214, 244)),
        dim: rgb((127, 132, 156)),
        border: rgb((69, 71, 90)),
        accent: rgb((137, 180, 250)),
        on_accent: rgb((30, 30, 46)),
        sel_bg: rgb((49, 50, 68)),
        bright: rgb((255, 255, 255)),
        rx: rgb((166, 227, 161)),
        tx: rgb((203, 166, 247)),
        rx_bar: rgb((44, 70, 54)),
        tx_bar: rgb((66, 52, 90)),
        warn: rgb((249, 226, 175)),
        err: rgb((243, 139, 168)),
        rx_grad: [(166, 227, 161), (249, 226, 175), (243, 139, 168)],
        tx_grad: [(137, 220, 235), (203, 166, 247), (245, 194, 231)],
        heat_grad: [(116, 199, 236), (249, 226, 175), (243, 139, 168)],
        land: rgb((69, 71, 90)),
    },
    Theme {
        name: "solarized-light",
        bg: Some(rgb((253, 246, 227))),
        fg: rgb((60, 76, 82)),
        dim: rgb((130, 145, 148)),
        border: rgb((200, 196, 180)),
        accent: rgb((38, 139, 210)),
        on_accent: rgb((253, 246, 227)),
        sel_bg: rgb((235, 228, 205)),
        bright: rgb((7, 54, 66)),
        rx: rgb((95, 135, 0)),
        tx: rgb((108, 113, 196)),
        rx_bar: rgb((222, 232, 196)),
        tx_bar: rgb((226, 224, 242)),
        warn: rgb((203, 75, 22)),
        err: rgb((220, 50, 47)),
        rx_grad: [(133, 153, 0), (181, 137, 0), (220, 50, 47)],
        tx_grad: [(42, 161, 152), (108, 113, 196), (211, 54, 130)],
        heat_grad: [(38, 139, 210), (181, 137, 0), (220, 50, 47)],
        land: rgb((222, 214, 190)),
    },
    Theme {
        name: "mono",
        bg: None,
        fg: rgb((220, 220, 220)),
        dim: rgb((130, 130, 130)),
        border: rgb((80, 80, 80)),
        accent: rgb((240, 240, 240)),
        on_accent: Color::Black,
        sel_bg: rgb((50, 50, 50)),
        bright: rgb((255, 255, 255)),
        rx: rgb((230, 230, 230)),
        tx: rgb((160, 160, 160)),
        rx_bar: rgb((62, 62, 62)),
        tx_bar: rgb((44, 44, 44)),
        warn: rgb((255, 255, 255)),
        err: rgb((255, 255, 255)),
        rx_grad: [(110, 110, 110), (190, 190, 190), (255, 255, 255)],
        tx_grad: [(90, 90, 90), (160, 160, 160), (230, 230, 230)],
        heat_grad: [(90, 90, 90), (180, 180, 180), (255, 255, 255)],
        land: rgb((58, 58, 58)),
    },
    // The 16 ANSI colours only, for terminals without true colour (the
    // terminal's own palette decides what they look like).
    Theme {
        name: "tty",
        bg: None,
        fg: Color::Reset,
        dim: Color::DarkGray,
        border: Color::DarkGray,
        accent: Color::Cyan,
        on_accent: Color::Black,
        sel_bg: Color::Blue,
        bright: Color::White,
        rx: Color::Green,
        tx: Color::Magenta,
        rx_bar: Color::DarkGray,
        tx_bar: Color::DarkGray,
        warn: Color::Yellow,
        err: Color::Red,
        rx_grad: [(0, 0, 0); 3],
        tx_grad: [(0, 0, 0); 3],
        heat_grad: [(0, 0, 0); 3],
        land: Color::DarkGray,
    },
];

static CUR: AtomicUsize = AtomicUsize::new(0);

/// The active theme.
pub fn th() -> &'static Theme {
    &THEMES[CUR.load(Ordering::Relaxed) % THEMES.len()]
}

pub fn set(i: usize) {
    CUR.store(i % THEMES.len(), Ordering::Relaxed);
}

pub fn index() -> usize {
    CUR.load(Ordering::Relaxed)
}

pub fn find(name: &str) -> Option<usize> {
    THEMES.iter().position(|t| t.name == name)
}

pub fn names() -> String {
    THEMES.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
}

impl Theme {
    pub fn is_tty(&self) -> bool {
        self.name == "tty"
    }
}
