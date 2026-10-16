//! Keyboard state machine: turns client key events into evdev key presses, xkb modifier masks
//! and keymap uploads. Backend-independent so the mapping is testable off Linux.

use std::collections::HashMap;

use super::evdev::*;
use super::keysym::{self, ModKey, Modifier};
use super::kvk;

pub const MOD_SHIFT: u32 = 0x01;
pub const MOD_LOCK: u32 = 0x02;
pub const MOD_CTRL: u32 = 0x04;
pub const MOD_ALT: u32 = 0x08;
pub const MOD_NUM: u32 = 0x10;
pub const MOD_SUPER: u32 = 0x40;
pub const MOD_LEVEL3: u32 = 0x80;

/// evdev codes with no meaning in the `evdev` xkb keycodes, rebound on demand to keysyms the
/// base layout can't type.
const SLOT_CODES: [u32; 5] = [195, 196, 197, 198, 199];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModTarget {
    Ctrl,
    Super,
    Alt,
    AltGr,
}

#[derive(Clone, Debug)]
pub struct KeyboardConfig {
    pub command: ModTarget,
    pub option: ModTarget,
    pub layout: String,
    pub variant: String,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            command: ModTarget::Super,
            option: ModTarget::Alt,
            layout: "us".into(),
            variant: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KbAction {
    Keymap(String),
    Key { code: u32, down: bool },
    Modifiers { depressed: u32, locked: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum KeyId {
    Sym(u32),
    Kvk(u16),
}

struct Slot {
    code: u32,
    keysym: Option<u32>,
    used: u64,
}

pub struct KeyboardEngine {
    cfg: KeyboardConfig,
    pressed: HashMap<KeyId, u32>,
    refs: HashMap<u32, u32>,
    locked: u32,
    slots: Vec<Slot>,
    clock: u64,
}

impl KeyboardEngine {
    pub fn new(cfg: KeyboardConfig) -> Self {
        let slots = SLOT_CODES
            .iter()
            .map(|&code| Slot {
                code,
                keysym: None,
                used: 0,
            })
            .collect();
        Self {
            cfg,
            pressed: HashMap::new(),
            refs: HashMap::new(),
            locked: 0,
            slots,
            clock: 0,
        }
    }

    /// Actions to run once after creating the virtual keyboard.
    pub fn init(&self) -> Vec<KbAction> {
        vec![KbAction::Keymap(self.keymap()), self.mods()]
    }

    pub fn keymap(&self) -> String {
        let slots: Vec<(u32, u32)> = self
            .slots
            .iter()
            .filter_map(|s| s.keysym.map(|k| (s.code, k)))
            .collect();
        keymap_text(&self.cfg, &slots)
    }

    /// `mac_keycode` is set when the client asked for keycode use (flag bits in msgs 16/23-12).
    pub fn key(&mut self, down: bool, keysym: u32, mac_keycode: Option<u16>) -> Vec<KbAction> {
        if mac_keycode == Some(kvk::KVK_FUNCTION) {
            return vec![];
        }
        let id = match mac_keycode {
            Some(k) if kvk::modifier(k).is_some() || kvk::to_evdev(k).is_some() => KeyId::Kvk(k),
            _ => KeyId::Sym(keysym::fold(keysym)),
        };
        if down {
            self.press(id, keysym)
        } else {
            self.release(id)
        }
    }

    pub fn release_all(&mut self) -> Vec<KbAction> {
        let mut out: Vec<KbAction> = self
            .refs
            .drain()
            .filter(|&(_, n)| n > 0)
            .map(|(code, _)| KbAction::Key { code, down: false })
            .collect();
        self.pressed.clear();
        out.push(self.mods());
        out
    }

    pub fn set_layout(&mut self, layout: &str, variant: &str) -> Vec<KbAction> {
        if self.cfg.layout == layout && self.cfg.variant == variant {
            return vec![];
        }
        let mut out = self.release_all();
        self.cfg.layout = layout.into();
        self.cfg.variant = variant.into();
        out.insert(out.len() - 1, KbAction::Keymap(self.keymap()));
        out
    }

    fn press(&mut self, id: KeyId, keysym: u32) -> Vec<KbAction> {
        if self.pressed.contains_key(&id) {
            return vec![];
        }
        let mut out = Vec::new();
        let (code, want_shift) = match self.resolve(id, keysym, &mut out) {
            Some(r) => r,
            None => return out,
        };
        self.pressed.insert(id, code);
        let n = self.refs.entry(code).or_insert(0);
        *n += 1;
        if *n > 1 {
            return out;
        }
        let before = self.depressed();
        let have_shift = before & MOD_SHIFT != 0;
        let toggle = want_shift.is_some_and(|w| w != have_shift);
        if toggle {
            out.push(KbAction::Modifiers {
                depressed: before ^ MOD_SHIFT,
                locked: self.locked,
            });
        }
        out.push(KbAction::Key { code, down: true });
        match code {
            KEY_CAPSLOCK => self.locked ^= MOD_LOCK,
            KEY_NUMLOCK => self.locked ^= MOD_NUM,
            _ => {}
        }
        if toggle || self.is_mod_code(code) || matches!(code, KEY_CAPSLOCK | KEY_NUMLOCK) {
            out.push(self.mods());
        }
        out
    }

    fn release(&mut self, id: KeyId) -> Vec<KbAction> {
        let Some(code) = self.pressed.remove(&id) else {
            return vec![];
        };
        let n = self.refs.entry(code).or_insert(1);
        *n -= 1;
        if *n > 0 {
            return vec![];
        }
        self.refs.remove(&code);
        let mut out = vec![KbAction::Key { code, down: false }];
        if self.is_mod_code(code) {
            out.push(self.mods());
        }
        out
    }

    /// evdev code plus the Shift state the keysym needs (`None` = whatever is held).
    fn resolve(
        &mut self,
        id: KeyId,
        keysym: u32,
        out: &mut Vec<KbAction>,
    ) -> Option<(u32, Option<bool>)> {
        if let KeyId::Kvk(k) = id {
            if let Some(m) = kvk::modifier(k) {
                return Some((self.modifier_code(m), None));
            }
            return kvk::to_evdev(k).map(|c| (c, None));
        }
        if let Some(m) = keysym::modifier(keysym) {
            return Some((self.modifier_code(m), None));
        }
        if let Some(c) = keysym::fixed(keysym) {
            return Some((c, None));
        }
        if self.cfg.layout == "us"
            && self.cfg.variant.is_empty()
            && let Some((c, shift)) = keysym::us(keysym)
        {
            return Some((c, Some(shift)));
        }
        self.slot_for(keysym, out).map(|c| (c, None))
    }

    fn slot_for(&mut self, keysym: u32, out: &mut Vec<KbAction>) -> Option<u32> {
        self.clock += 1;
        if let Some(s) = self.slots.iter_mut().find(|s| s.keysym == Some(keysym)) {
            s.used = self.clock;
            return Some(s.code);
        }
        let refs = &self.refs;
        let s = self
            .slots
            .iter_mut()
            .filter(|s| !refs.contains_key(&s.code))
            .min_by_key(|s| s.used)?;
        s.keysym = Some(keysym);
        s.used = self.clock;
        let code = s.code;
        out.push(KbAction::Keymap(self.keymap()));
        out.push(self.mods());
        Some(code)
    }

    fn modifier_code(&self, m: Modifier) -> u32 {
        let target = match m.key {
            ModKey::Shift => {
                return if m.right {
                    KEY_RIGHTSHIFT
                } else {
                    KEY_LEFTSHIFT
                };
            }
            ModKey::CapsLock => return KEY_CAPSLOCK,
            ModKey::Control => ModTarget::Ctrl,
            ModKey::Command => self.cfg.command,
            ModKey::Option => self.cfg.option,
        };
        match (target, m.right) {
            (ModTarget::Ctrl, false) => KEY_LEFTCTRL,
            (ModTarget::Ctrl, true) => KEY_RIGHTCTRL,
            (ModTarget::Super, false) => KEY_LEFTMETA,
            (ModTarget::Super, true) => KEY_RIGHTMETA,
            (ModTarget::Alt, false) => KEY_LEFTALT,
            (ModTarget::Alt, true) => KEY_RIGHTALT,
            (ModTarget::AltGr, _) => KEY_RIGHTALT,
        }
    }

    fn mod_mask(&self, code: u32) -> u32 {
        match code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => MOD_SHIFT,
            KEY_LEFTCTRL | KEY_RIGHTCTRL => MOD_CTRL,
            KEY_LEFTMETA | KEY_RIGHTMETA => MOD_SUPER,
            KEY_LEFTALT => MOD_ALT,
            KEY_RIGHTALT if self.altgr() => MOD_LEVEL3,
            KEY_RIGHTALT => MOD_ALT,
            _ => 0,
        }
    }

    fn is_mod_code(&self, code: u32) -> bool {
        self.mod_mask(code) != 0
    }

    fn altgr(&self) -> bool {
        self.cfg.command == ModTarget::AltGr || self.cfg.option == ModTarget::AltGr
    }

    fn depressed(&self) -> u32 {
        self.refs.keys().fold(0, |m, &c| m | self.mod_mask(c))
    }

    fn mods(&self) -> KbAction {
        KbAction::Modifiers {
            depressed: self.depressed(),
            locked: self.locked,
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .collect()
}

/// xkb keymap the compositor compiles against its own xkeyboard-config data.
pub fn keymap_text(cfg: &KeyboardConfig, slots: &[(u32, u32)]) -> String {
    let layout = sanitize(&cfg.layout);
    let variant = sanitize(&cfg.variant);
    let layout = if layout.is_empty() {
        "us".to_string()
    } else {
        layout
    };
    let base = if variant.is_empty() {
        layout
    } else {
        format!("{layout}({variant})")
    };
    let level3 = if cfg.command == ModTarget::AltGr || cfg.option == ModTarget::AltGr {
        "+level3(ralt_switch)"
    } else {
        ""
    };
    let mut keycodes = String::new();
    let mut symbols = String::new();
    for &(code, sym) in slots {
        keycodes.push_str(&format!("    <ARD{code}> = {};\n", code + 8));
        symbols.push_str(&format!(
            "    key <ARD{code}> {{ type = \"ONE_LEVEL\", symbols[Group1] = [ 0x{sym:08x} ] }};\n"
        ));
    }
    format!(
        "xkb_keymap {{\n\
         xkb_keycodes \"wayshare\" {{\n    include \"evdev+aliases(qwerty)\"\n{keycodes}}};\n\
         xkb_types \"wayshare\" {{ include \"complete\" }};\n\
         xkb_compat \"wayshare\" {{ include \"complete\" }};\n\
         xkb_symbols \"wayshare\" {{\n    include \"pc+{base}+inet(evdev){level3}\"\n{symbols}}};\n\
         }};\n"
    )
}

/// macOS input source ID (client msg 26) -> xkb (layout, variant).
pub fn layout_for_input_source(id: &str) -> Option<(&'static str, &'static str)> {
    let name = id.strip_prefix("com.apple.keylayout.")?;
    Some(match name {
        "US" | "ABC" => ("us", ""),
        "USInternational-PC" | "USExtended" | "ABC-Extended" => ("us", "intl"),
        "Dvorak" => ("us", "dvorak"),
        "Colemak" => ("us", "colemak"),
        "British" | "British-PC" => ("gb", ""),
        "German" => ("de", "mac"),
        "French" => ("fr", "mac"),
        "Spanish" | "Spanish-ISO" => ("es", ""),
        "Italian" | "Italian-Pro" => ("it", "mac"),
        "Swedish" | "Swedish-Pro" => ("se", "mac"),
        "Norwegian" => ("no", "mac"),
        "Danish" => ("dk", "mac"),
        "Finnish" => ("fi", "mac"),
        "SwissGerman" => ("ch", "de_mac"),
        "SwissFrench" => ("ch", "fr_mac"),
        "Dutch" => ("nl", "mac"),
        "Portuguese" => ("pt", "mac"),
        "Brazilian" | "Brazilian-ABNT2" => ("br", ""),
        "Canadian-CSA" => ("ca", ""),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use KbAction::*;

    fn eng() -> KeyboardEngine {
        KeyboardEngine::new(KeyboardConfig::default())
    }

    fn keys(actions: &[KbAction]) -> Vec<(u32, bool)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Key { code, down } => Some((*code, *down)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn plain_letter() {
        let mut e = eng();
        assert_eq!(
            e.key(true, 'a' as u32, None),
            vec![Key {
                code: KEY_A,
                down: true
            }]
        );
        assert_eq!(
            e.key(false, 'a' as u32, None),
            vec![Key {
                code: KEY_A,
                down: false
            }]
        );
    }

    #[test]
    fn shifted_symbol_without_shift_held_gets_temporary_shift() {
        let mut e = eng();
        assert_eq!(
            e.key(true, '!' as u32, None),
            vec![
                Modifiers {
                    depressed: MOD_SHIFT,
                    locked: 0
                },
                Key {
                    code: KEY_1,
                    down: true
                },
                Modifiers {
                    depressed: 0,
                    locked: 0
                },
            ]
        );
    }

    #[test]
    fn shift_then_capital_needs_no_override() {
        let mut e = eng();
        assert_eq!(
            e.key(true, 0xffe1, None),
            vec![
                Key {
                    code: KEY_LEFTSHIFT,
                    down: true
                },
                Modifiers {
                    depressed: MOD_SHIFT,
                    locked: 0
                }
            ]
        );
        assert_eq!(
            e.key(true, 'A' as u32, None),
            vec![Key {
                code: KEY_A,
                down: true
            }]
        );
        // Release arrives lowercase after Shift went up first.
        e.key(false, 0xffe1, None);
        assert_eq!(
            e.key(false, 'a' as u32, None),
            vec![Key {
                code: KEY_A,
                down: false
            }]
        );
    }

    #[test]
    fn command_defaults_to_super_and_option_to_alt() {
        let mut e = eng();
        assert_eq!(keys(&e.key(true, 0xffe7, None)), vec![(KEY_LEFTMETA, true)]);
        assert_eq!(keys(&e.key(true, 0xffe9, None)), vec![(KEY_LEFTALT, true)]);
        assert_eq!(e.depressed(), MOD_SUPER | MOD_ALT);
        let mut e = KeyboardEngine::new(KeyboardConfig {
            command: ModTarget::Ctrl,
            ..Default::default()
        });
        assert_eq!(
            keys(&e.key(true, 0x37, Some(0x37))),
            vec![(KEY_LEFTCTRL, true)]
        );
        assert_eq!(e.depressed(), MOD_CTRL);
    }

    #[test]
    fn command_and_control_share_ctrl_refcounted() {
        let mut e = KeyboardEngine::new(KeyboardConfig {
            command: ModTarget::Ctrl,
            ..Default::default()
        });
        assert_eq!(keys(&e.key(true, 0xffe3, None)), vec![(KEY_LEFTCTRL, true)]);
        assert_eq!(keys(&e.key(true, 0xffe7, None)), vec![]);
        assert_eq!(keys(&e.key(false, 0xffe3, None)), vec![]);
        assert_eq!(e.depressed(), MOD_CTRL);
        assert_eq!(
            keys(&e.key(false, 0xffe7, None)),
            vec![(KEY_LEFTCTRL, false)]
        );
        assert_eq!(e.depressed(), 0);
    }

    #[test]
    fn mac_keycode_is_positional() {
        let mut e = eng();
        // kVK_ANSI_Q with whatever keysym the client's layout produced (here AZERTY 'a').
        assert_eq!(
            keys(&e.key(true, 'a' as u32, Some(0x0c))),
            vec![(KEY_Q, true)]
        );
        assert_eq!(
            keys(&e.key(false, 'a' as u32, Some(0x0c))),
            vec![(KEY_Q, false)]
        );
        assert!(e.key(true, 0, Some(kvk::KVK_FUNCTION)).is_empty());
    }

    #[test]
    fn unknown_mac_keycode_falls_back_to_keysym() {
        let mut e = eng();
        assert_eq!(
            keys(&e.key(true, 'z' as u32, Some(0x7f))),
            vec![(KEY_Z, true)]
        );
    }

    #[test]
    fn autorepeat_downs_are_ignored() {
        let mut e = eng();
        e.key(true, 'x' as u32, None);
        assert!(e.key(true, 'x' as u32, None).is_empty());
    }

    #[test]
    fn non_us_keysym_uses_slot_and_uploads_keymap() {
        let mut e = eng();
        let a = e.key(true, 0xe9, None);
        assert!(
            matches!(&a[0], Keymap(k) if k.contains("<ARD195> = 203;") && k.contains("0x000000e9"))
        );
        assert_eq!(keys(&a), vec![(195, true)]);
        e.key(false, 0xe9, None);
        // Reuse without another upload.
        assert_eq!(
            e.key(true, 0xe9, None),
            vec![Key {
                code: 195,
                down: true
            }]
        );
        e.key(false, 0xe9, None);
        let a = e.key(true, 0x1000263a, None);
        assert!(matches!(&a[0], Keymap(k) if k.contains("0x1000263a") && k.contains("0x000000e9")));
        assert_eq!(keys(&a), vec![(196, true)]);
    }

    #[test]
    fn slots_recycle_lru_and_skip_held() {
        let mut e = eng();
        e.key(true, 0x100, None); // held: slot 195
        for ks in 0x101..0x105 {
            e.key(true, ks, None);
            e.key(false, ks, None);
        }
        // All five used; the next one must evict 196 (oldest not held), not 195.
        assert_eq!(keys(&e.key(true, 0x200, None)), vec![(196, true)]);
    }

    #[test]
    fn caps_and_num_lock_toggle_locked_mask() {
        let mut e = eng();
        assert_eq!(
            e.key(true, 0xffe5, None).last(),
            Some(&Modifiers {
                depressed: 0,
                locked: MOD_LOCK
            })
        );
        e.key(false, 0xffe5, None);
        assert_eq!(
            e.key(true, 0xff7f, None).last(),
            Some(&Modifiers {
                depressed: 0,
                locked: MOD_LOCK | MOD_NUM
            })
        );
    }

    #[test]
    fn release_all_lifts_everything() {
        let mut e = eng();
        e.key(true, 0xffe1, None);
        e.key(true, 'b' as u32, None);
        let mut up = keys(&e.release_all());
        up.sort();
        assert_eq!(up, vec![(KEY_LEFTSHIFT, false), (KEY_B, false)]);
        assert_eq!(e.depressed(), 0);
    }

    #[test]
    fn non_us_layout_routes_printables_through_slots() {
        let mut e = eng();
        let a = e.set_layout("de", "mac");
        assert!(
            matches!(a.iter().find(|a| matches!(a, Keymap(_))), Some(Keymap(k)) if k.contains("pc+de(mac)+inet(evdev)"))
        );
        assert_eq!(keys(&e.key(true, 'z' as u32, None)), vec![(195, true)]);
        assert_eq!(keys(&e.key(true, 0xff0d, None)), vec![(KEY_ENTER, true)]);
    }

    #[test]
    fn keymap_text_shape() {
        let k = keymap_text(&KeyboardConfig::default(), &[]);
        assert!(k.contains("include \"pc+us+inet(evdev)\""));
        assert!(k.contains("include \"evdev+aliases(qwerty)\""));
        let altgr = KeyboardConfig {
            option: ModTarget::AltGr,
            ..Default::default()
        };
        assert!(keymap_text(&altgr, &[]).contains("+level3(ralt_switch)"));
        let evil = KeyboardConfig {
            layout: "us\"; };".into(),
            ..Default::default()
        };
        assert!(keymap_text(&evil, &[]).contains("pc+us+inet"));
    }

    #[test]
    fn input_sources() {
        assert_eq!(
            layout_for_input_source("com.apple.keylayout.US"),
            Some(("us", ""))
        );
        assert_eq!(
            layout_for_input_source("com.apple.keylayout.German"),
            Some(("de", "mac"))
        );
        assert_eq!(
            layout_for_input_source("com.apple.inputmethod.Kotoeri"),
            None
        );
    }
}
